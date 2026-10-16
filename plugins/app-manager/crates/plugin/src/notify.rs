//! The `notify` Lua package: an app's way to the notification center.
//!
//! `notify.post(title, body)` posts a notice and returns its ID;
//! `notify.ask(title, body)` asks the person and returns whether they
//! allowed it, `false` when the card was dismissed unanswered.

use crate::notices::Notifications;
use alloc::{boxed::Box, format, rc::Rc, string::String, vec::Vec};
use barracuda_appkit::BuiltinBackend;
use barracuda_vm_plugin::AppApi;
use core::{future::Future, pin::Pin};
use serde_json::Value;

/// The package's name.
pub const NOTIFY: &str = "notify";
/// Where the package stands in the app API.
pub const NOTIFY_API: AppApi = AppApi::Capability {
    since: 1,
    purpose: "发通知、向你确认",
};

/// Answers the `notify` package with the notification center.
pub(crate) struct Notify(pub(crate) Rc<Notifications>);

impl BuiltinBackend for Notify {
    fn call<'a>(
        &'a self,
        call: &'a str,
        args: Vec<Option<String>>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            let text = |index: usize, what: &str| {
                args.get(index)
                    .cloned()
                    .flatten()
                    .ok_or_else(|| format!("notify.{call} needs a {what}"))
            };
            match call {
                "post" => {
                    let title = text(0, "title")?;
                    let body = args.get(1).cloned().flatten().unwrap_or_default();
                    Ok(Value::from(self.0.post(&title, &body, Some("apps"))))
                }
                "ask" => {
                    let title = text(0, "title")?;
                    let body = args.get(1).cloned().flatten().unwrap_or_default();
                    let id = self.0.ask(&title, &body);
                    Ok(Value::Bool(self.0.answer(id).await.unwrap_or(false)))
                }
                _ => Err(format!("notify.{call} does not exist")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use alloc::string::ToString;
    use futures_lite::future::block_on;

    #[test]
    fn posts_and_asks_through_the_notification_center() {
        let notices = Rc::new(Notifications::new());
        let notify = Notify(Rc::clone(&notices));
        let posted = block_on(notify.call(
            "post",
            alloc::vec![Some("Done".to_string()), Some("Lamp".to_string())],
        ))
        .unwrap();
        assert_eq!(notices.list()[0].title, "Done");
        assert_eq!(posted, Value::from(notices.list()[0].id));
        let answered = block_on(embassy_futures::join::join(
            notify.call("ask", alloc::vec![Some("Turn on?".to_string()), None]),
            async {
                let question = notices
                    .list()
                    .into_iter()
                    .find(|notice| notice.question)
                    .unwrap();
                notices.decide(question.id, true);
            },
        ))
        .0;
        assert_eq!(answered, Ok(Value::Bool(true)));
        assert!(block_on(notify.call("post", alloc::vec![None])).is_err());
        assert!(block_on(notify.call("shout", Vec::new())).is_err());
    }
}
