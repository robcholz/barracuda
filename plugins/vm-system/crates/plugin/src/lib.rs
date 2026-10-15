//! The `system` Lua package: the device's settings and state, for the system
//! apps that show and change them.
//!
//! Only system apps receive this package; the `app` Plugin refuses an
//! installable app that asks for it. Each call goes to this Plugin's service
//! task, which owns the capabilities it reads and changes.

#![no_std]

extern crate alloc;

mod backend;
mod words;

use alloc::{
    boxed::Box,
    rc::Rc,
    string::{String, ToString},
};
use core::{future::Future, pin::Pin};

use async_channel::{Receiver, Sender};
use barracuda_agent_plugin::AgentRuntime;
use barracuda_app_plugin::{AppManager, SYSTEM_PACKAGE};
use barracuda_plugin::api::{PluginContext, TargetIdentity};
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
    PluginStartContext, PluginTaskToken,
};
use barracuda_runtime_utils::{oneshot, unordered::Unordered};
use barracuda_shell_plugin::{AgentStatus, Notifications, Preferences};
use barracuda_time_plugin::UtcClock;
use barracuda_vm_plugin::{
    Context, Error, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package, Result,
};
use barracuda_wifi_plugin::WifiControl;
use barracuda_workflow_plugin::WorkflowService;
use embassy_futures::select::{Either, Either3, select, select3};
use futures_util::stream::StreamExt as _;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;
use serde_json::Value;

pub use backend::{AREAS, Backend, Call, MAX_FILES, Setting, VERSION};

/// Calls queued for the service task before `system` calls fail as busy.
const QUEUE_DEPTH: usize = 4;

/// Registers the `system` Lua package and serves its calls.
#[barracuda_plugin::macros::plugin]
pub struct VmSystemPlugin {
    target_identity: TargetIdentity,
    runtime: Option<(Rc<Backend>, Receiver<Job>, Arc<AtomicBool>)>,
}

impl VmSystemPlugin {
    /// Creates an unregistered adapter for the selected Target.
    #[must_use]
    pub const fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            target_identity: context.target_identity,
            runtime: None,
        }
    }
}

/// The Plugin started before it registered.
#[derive(Debug)]
struct SystemUnavailable;

impl core::fmt::Display for SystemUnavailable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("the system package is unavailable")
    }
}

impl core::error::Error for SystemUnavailable {}

impl Plugin for VmSystemPlugin {
    // Storage sizes are read from every Plugin's media tree.
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Inspect);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let backend = Backend {
            identity: self.target_identity,
            filesystem: context.filesystem()?.clone(),
            preferences: context.require::<Preferences>("shell")?,
            notices: context.require::<Notifications>("shell")?,
            agent_status: context.require::<AgentStatus>("shell")?,
            apps: (*context.require::<AppManager>("app")?).clone(),
            wifi: context.require::<WifiControl>("wifi")?,
            agent: context.require::<AgentRuntime>("agent")?,
            workflows: context.require::<WorkflowService>("workflow")?,
            clock: context.require::<UtcClock>("time")?,
        };
        let packages = context.require::<LuaPackageRegistry>("vm")?;
        let (package, jobs) = SystemPackage::new();
        let active = Arc::clone(&package.active);
        let registration = packages
            .register(package)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        self.runtime = Some((Rc::new(backend), jobs, active));
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let (backend, jobs, active) = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(SystemUnavailable))?;
        let task = system_task(backend, jobs, active, context.task_token())
            .map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

#[embassy_executor::task]
async fn system_task(
    backend: Rc<Backend>,
    jobs: Receiver<Job>,
    active: Arc<AtomicBool>,
    cancellation: PluginTaskToken,
) {
    serve(backend, &jobs, cancellation).await;
    active.store(false, Ordering::Release);
    jobs.close();
}

type Answer = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// Answers calls, several at once so a question waiting for the person
/// holds up no other call, until cancelled.
async fn serve(backend: Rc<Backend>, jobs: &Receiver<Job>, cancellation: PluginTaskToken) {
    let mut answers = Unordered::<Answer>::new();
    let answer = |job: Job| -> Answer {
        let backend = Rc::clone(&backend);
        Box::pin(async move {
            let reply = backend.call(job.call).await;
            let _gone = job.reply.send(reply);
        })
    };
    loop {
        if answers.is_empty() {
            match select(cancellation.cancelled(), jobs.recv()).await {
                Either::First(()) | Either::Second(Err(_)) => return,
                Either::Second(Ok(job)) => answers.push(answer(job)),
            }
        } else {
            match select3(cancellation.cancelled(), jobs.recv(), answers.next()).await {
                Either3::First(()) | Either3::Second(Err(_)) => return,
                Either3::Second(Ok(job)) => answers.push(answer(job)),
                Either3::Third(_) => {}
            }
        }
    }
}

/// One call from Lua and where its answer goes.
struct Job {
    call: Call,
    reply: oneshot::Sender<core::result::Result<Value, String>>,
}

/// The `system` package as Lua sees it; its calls run in the service task.
struct SystemPackage {
    jobs: Sender<Job>,
    active: Arc<AtomicBool>,
}

impl SystemPackage {
    fn new() -> (Self, Receiver<Job>) {
        let (jobs, receiver) = async_channel::bounded(QUEUE_DEPTH);
        (
            Self {
                jobs,
                active: Arc::new(AtomicBool::new(true)),
            },
            receiver,
        )
    }
}

impl Package for SystemPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let jobs = self.jobs.clone();
        let active = Arc::clone(&self.active);
        lua.register_lib(SYSTEM_PACKAGE, move |system| {
            system.register_async_with(
                "__call",
                move |lua,
                      (name, first, second, third): (
                    String,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                )| {
                    let sent = if active.load(Ordering::Acquire) {
                        Call::parse(&name, first, second, third).and_then(|call| {
                            let (reply, answer) = oneshot::channel();
                            jobs.try_send(Job { call, reply })
                                .map(|()| answer)
                                .map_err(|error| {
                                    String::from(if error.is_closed() {
                                        "the system package has been revoked"
                                    } else {
                                        "the system is busy"
                                    })
                                })
                        })
                    } else {
                        Err(String::from("the system package has been revoked"))
                    };
                    let _lua = lua;
                    async move {
                        let reply = match sent {
                            Ok(answer) => answer
                                .await
                                .unwrap_or_else(|_| Err(String::from("the system stopped"))),
                            Err(error) => Err(error),
                        };
                        Some(reply.map(|value| value.to_string()).map_err(Error::runtime))
                    }
                },
            )?;
            system.register_with("__decode", |lua, json: String| {
                Some(
                    serde_json::from_str::<Value>(&json)
                        .map_err(|_| Error::runtime("the system answered badly"))
                        .and_then(|value| decode(lua, value)),
                )
            })
        })?;
        lua.load(ADAPTER).exec()
    }
}

impl LuaPackage for SystemPackage {
    fn name(&self) -> &'static str {
        SYSTEM_PACKAGE
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

fn decode(lua: &mut Context<'_>, value: Value) -> Result<LuaValue> {
    Ok(match value {
        Value::Null => LuaValue::Nil,
        Value::Bool(value) => LuaValue::Boolean(value),
        Value::Number(number) => match number.as_i64() {
            Some(value) => LuaValue::Integer(value),
            None => LuaValue::Number(number.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(value) => LuaValue::String(value),
        Value::Array(items) => {
            let table = lua.create_table()?;
            for (index, item) in (1_i64..).zip(items) {
                table.set(index, decode(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
        Value::Object(object) => {
            let table = lua.create_table()?;
            for (key, item) in object {
                table.set(key, decode(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
    })
}

/// Builds the public `system` table on the native call.
const ADAPTER: &str = r##"
local system = require("system")
local call, decode = system.__call, system.__decode
system.__call, system.__decode = nil, nil

local function run(name, first, second, third)
  local json, message = call(name, first, second, third)
  if json == nil then return nil, message end
  return decode(json)
end

local function text(value)
  if value == nil then return nil end
  return tostring(value)
end

function system.status() return run("status") end
function system.scan() return run("scan") end
function system.set(key, value) return run("set", key, text(value)) end
function system.apps() return run("apps") end
function system.sessions() return run("sessions") end
function system.workflows() return run("workflows") end
function system.storage() return run("storage") end
function system.files(area) return run("files", area) end
function system.notify(title, body, icon) return run("notify", title, body, icon) end
function system.ask(title, body) return run("ask", title, body) end
function system.agent(state, question, answer)
  return run("agent", state, question, answer)
end
"##;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    extern crate std;

    use super::*;
    use barracuda_plugin::manager::PluginDeclaration;

    #[test]
    fn declaration_names_what_the_package_reads() {
        assert_eq!(VmSystemPlugin::ID, "vm-system");
        assert_eq!(
            VmSystemPlugin::DEPENDS_ON,
            &["vm", "ui", "shell", "app", "wifi", "agent", "workflow", "time"]
        );
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let (package, _jobs) = SystemPackage::new();
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }

    #[test]
    fn calls_reach_the_service_task_and_bad_ones_fail_in_lua() {
        let (package, jobs) = SystemPackage::new();
        let mut lua = Lua::new().expect("create Lua");
        package
            .install(&mut lua)
            .expect("install the system package");
        let (ok, message): (bool, String) = lua
            .load("local ok, message = require('system').set('loudness', 3) return ok == nil, message")
            .eval()
            .expect("a bad setting is an error value");
        assert!(ok, "{message}");
        assert!(message.contains("loudness"), "{message}");
        assert!(jobs.is_empty());
        let functions: bool = lua
            .load(
                "local system = require('system') \
                 return system.__call == nil and type(system.status) == 'function'",
            )
            .eval()
            .expect("read the package");
        assert!(functions);
    }
}
