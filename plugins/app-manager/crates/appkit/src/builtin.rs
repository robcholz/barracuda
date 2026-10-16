//! Builtin apps: system apps that other Plugins ship, each with an optional
//! Lua package that only builtin apps receive.
//!
//! A builtin app Plugin hands [`AppManager::add_builtin`] its container and,
//! when its app needs the device's state, a [`BuiltinBackend`] and the name of
//! the package that reaches it. A call from Lua, `require("<name>").<call>(…)`,
//! queues a job; the app manager's task answers it with the backend, so the
//! backend may hold the Plugin's single-task capabilities.
//!
//! [`AppManager::add_builtin`]: crate::AppManager::add_builtin

use crate::package::decode;
use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use async_channel::{Receiver, Sender};
use barracuda_runtime_utils::{oneshot, unordered::Unordered};
use barracuda_vfs::ScopedVfs;
use barracuda_vm_plugin::{Error, Lua, LuaPackage, Package, Result};
use core::{cell::RefCell, future::Future, pin::Pin};
use futures_util::stream::StreamExt as _;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;
use serde_json::Value;

/// Calls to builtin backends queued at once; further ones fail as busy.
pub const MAX_BUILTIN_CALLS: usize = 8;
/// The most arguments a call to a builtin backend passes.
pub const MAX_BUILTIN_ARGS: usize = 4;

/// What a builtin app's Lua package answers with, on the app manager's task.
pub trait BuiltinBackend {
    /// Answers `call` with its arguments, each a string or absent, with the
    /// value Lua receives or the message it gets as an error.
    fn call<'a>(
        &'a self,
        call: &'a str,
        args: Vec<Option<String>>,
    ) -> Pin<Box<dyn Future<Output = core::result::Result<Value, String>> + 'a>>;
}

/// A builtin app's Lua package: its name and what answers it.
pub struct BuiltinPackage {
    /// The package name; installable apps may never ask for it.
    pub name: &'static str,
    /// Answers its calls.
    pub backend: Rc<dyn BuiltinBackend>,
}

/// A system app another Plugin ships.
pub struct BuiltinApp {
    /// The app ID, equal to the `id` in its manifest.
    pub id: &'static str,
    /// The shipping Plugin's filesystem, with the container at
    /// [`BUILTIN_APP`](crate::BUILTIN_APP).
    pub filesystem: ScopedVfs,
    /// What the system UI opens this app for, such as `wifi` for the Wi-Fi
    /// tile of the control center.
    pub opens: &'static [&'static str],
    /// The package its program requires, if any.
    pub package: Option<BuiltinPackage>,
}

/// One call from Lua and where its answer goes.
pub(crate) struct Job {
    backend: usize,
    call: String,
    args: Vec<Option<String>>,
    reply: oneshot::Sender<core::result::Result<Value, String>>,
}

/// The builtin backends and the queue of calls to them.
pub(crate) struct Backends {
    backends: RefCell<Vec<Rc<dyn BuiltinBackend>>>,
    jobs: Sender<Job>,
    queue: Receiver<Job>,
}

impl Backends {
    pub(crate) fn new() -> Self {
        let (jobs, queue) = async_channel::bounded(MAX_BUILTIN_CALLS);
        Self {
            backends: RefCell::new(Vec::new()),
            jobs,
            queue,
        }
    }

    /// Adds `backend` and returns the Lua package that reaches it as `name`.
    pub(crate) fn add(
        &self,
        name: &'static str,
        backend: Rc<dyn BuiltinBackend>,
    ) -> BackendPackage {
        let mut backends = self.backends.borrow_mut();
        backends.push(backend);
        BackendPackage {
            name,
            backend: backends.len() - 1,
            jobs: self.jobs.clone(),
            active: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Answers calls, several at once so a call that waits on the person
    /// holds up no other, until the queue closes.
    pub(crate) async fn serve(&self) {
        let mut answers = Unordered::<Pin<Box<dyn Future<Output = ()> + '_>>>::new();
        loop {
            let job = if answers.is_empty() {
                self.queue.recv().await.ok()
            } else {
                match embassy_futures::select::select(self.queue.recv(), answers.next()).await {
                    embassy_futures::select::Either::First(job) => job.ok(),
                    embassy_futures::select::Either::Second(_) => continue,
                }
            };
            let Some(job) = job else {
                return;
            };
            let backend = self.backends.borrow().get(job.backend).cloned();
            answers.push(Box::pin(async move {
                let answer = match backend {
                    Some(backend) => backend.call(&job.call, job.args).await,
                    None => Err(String::from("the builtin app is gone")),
                };
                let _gone = job.reply.send(answer);
            }));
        }
    }
}

/// A builtin app's Lua package, as the VM installs it.
pub(crate) struct BackendPackage {
    name: &'static str,
    backend: usize,
    jobs: Sender<Job>,
    active: Arc<AtomicBool>,
}

impl Package for BackendPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let jobs = self.jobs.clone();
        let active = Arc::clone(&self.active);
        let backend = self.backend;
        lua.register_lib(self.name, move |package| {
            package.register_async_with(
                "__call",
                move |_lua,
                      (call, first, second, third, fourth): (
                    String,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                )| {
                    let queued = if active.load(Ordering::Acquire) {
                        let (reply, answer) = oneshot::channel();
                        let job = Job {
                            backend,
                            call,
                            args: alloc::vec![first, second, third, fourth],
                            reply,
                        };
                        jobs.try_send(job).map(|()| answer).map_err(|error| {
                            String::from(if error.is_closed() {
                                "the app manager stopped"
                            } else {
                                "the system is busy"
                            })
                        })
                    } else {
                        Err(String::from("the package has been revoked"))
                    };
                    async move {
                        let answer = match queued {
                            Ok(answer) => answer
                                .await
                                .unwrap_or_else(|_| Err(String::from("the app manager stopped"))),
                            Err(error) => Err(error),
                        };
                        Some(
                            answer
                                .map(|value| value.to_string())
                                .map_err(Error::runtime),
                        )
                    }
                },
            )?;
            package.register_with("__decode", |lua, json: String| {
                Some(
                    serde_json::from_str::<Value>(&json)
                        .map_err(|_| Error::runtime("the answer is not JSON"))
                        .and_then(|value| decode(lua, value)),
                )
            })
        })?;
        let adapter = format!("local package = require({:?})\n{ADAPTER}", self.name);
        lua.load(&adapter).exec()
    }
}

impl LuaPackage for BackendPackage {
    fn name(&self) -> &'static str {
        self.name
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

/// Turns every field of the package table into a call to its backend:
/// arguments become strings, and the answer comes back as Lua values, or as
/// `nil` and a message.
const ADAPTER: &str = r##"
local call, decode = package.__call, package.__decode
package.__call, package.__decode = nil, nil
setmetatable(package, {
  __index = function(_, name)
    return function(...)
      local count = select("#", ...)
      if count > 4 then error("at most 4 arguments", 2) end
      local args = { ... }
      for index = 1, count do
        if args[index] ~= nil then args[index] = tostring(args[index]) end
      end
      local json, message = call(name, args[1], args[2], args[3], args[4])
      if json == nil then return nil, message end
      return decode(json)
    end
  end,
})
"##;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    extern crate std;

    use super::*;
    use futures_lite::future::block_on;
    use serde_json::json;

    struct Echo;

    impl BuiltinBackend for Echo {
        fn call<'a>(
            &'a self,
            call: &'a str,
            args: Vec<Option<String>>,
        ) -> Pin<Box<dyn Future<Output = core::result::Result<Value, String>> + 'a>> {
            Box::pin(async move {
                match call {
                    "echo" => Ok(json!({ "call": call, "first": args[0], "second": args[1] })),
                    _ => Err(format!("no call {call}")),
                }
            })
        }
    }

    #[test]
    fn calls_queue_for_the_backend_and_answer_its_value() {
        let backends = Backends::new();
        let package = backends.add("echo", Rc::new(Echo));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install the package");
        assert!(backends.queue.try_recv().is_err());
        // The job a Lua call queues, answered by the serving loop.
        let answer = block_on(async {
            let (reply, answer) = oneshot::channel();
            backends
                .jobs
                .try_send(Job {
                    backend: 0,
                    call: String::from("echo"),
                    args: alloc::vec![Some(String::from("1")), None, None, None],
                    reply,
                })
                .unwrap();
            let serve = backends.serve();
            let answered = async {
                let answer = answer.await.unwrap();
                backends.jobs.close();
                answer
            };
            embassy_futures::join::join(serve, answered).await.1
        });
        assert_eq!(
            answer,
            Ok(json!({ "call": "echo", "first": "1", "second": null }))
        );
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let backends = Backends::new();
        let package = backends.add("echo", Rc::new(Echo));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
