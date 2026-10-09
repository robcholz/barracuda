//! `POST /api/gateway/bluebubbles/hook/<secret>`: BlueBubbles webhook
//! deliveries.
//!
//! The endpoint only checks a delivery and queues it; the receive session
//! publishes it. Every delivery with the right secret is answered 204 at
//! once, whatever happens to it, so the server is never blocked.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use barracuda_imessage_gateway_channel::{ChannelControl, JSON_CONTENT_TYPE};
use barracuda_plugin::manager::PluginStorage;
use barracuda_webserver_plugin::{HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse};
use bluebubbles::inbound::{parse_webhook, WebhookPayload};
use http_client::embedded_nal_async::{Dns, TcpConnect};

use crate::channel::BlueBubblesChannel;
use crate::state::{Offer, HOOK_PATH};

/// Largest webhook body handled; larger deliveries are counted as lost. The
/// webserver's request buffer (8 KiB with the head) rejects larger ones
/// before they reach the endpoint.
pub(crate) const WEBHOOK_BODY_MAX: usize = 8 * 1024;

/// What happened to one delivery, for logs and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Wrong path or secret, or receiving is off: 404.
    NotFound,
    /// Right secret, wrong method: 405.
    WrongMethod,
    /// Over [`WEBHOOK_BODY_MAX`]; counted as lost.
    TooLarge,
    /// Not a BlueBubbles event.
    Invalid,
    /// Another event type.
    OtherEvent,
    /// Sent from this Mac's account.
    FromMe,
    /// Already handled or queued.
    Duplicate,
    /// Queued for the session.
    Queued,
    /// The inbox is full; counted as lost and caught up later.
    Full,
}

impl Delivery {
    const fn status(self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::WrongMethod => 405,
            _ => 204,
        }
    }
}

/// The webhook endpoint, served under [`HOOK_PATH`] as a prefix.
pub(crate) struct WebhookEndpoint<Storage, T: 'static, D: 'static>(
    pub(crate) Rc<BlueBubblesChannel<Storage, T, D>>,
);

impl<Storage, T, D> HttpEndpoint for WebhookEndpoint<Storage, T, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        let delivery = self
            .0
            .deliver(request.method(), request.path(), request.body());
        log::debug!("BlueBubbles webhook delivery: {delivery:?}");
        Box::pin(async move {
            let body = match delivery.status() {
                404 => Vec::from(&br#"{"error":"not_found"}"#[..]),
                405 => Vec::from(&br#"{"error":"method_not_allowed"}"#[..]),
                _ => Vec::new(),
            };
            HttpResponse::new(delivery.status(), JSON_CONTENT_TYPE, body)
        })
    }
}

impl<Storage, T, D> BlueBubblesChannel<Storage, T, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    /// Checks one delivery and queues a new incoming message.
    pub(crate) fn deliver(&self, method: HttpMethod, path: &str, body: &[u8]) -> Delivery {
        let secret = path
            .strip_prefix(HOOK_PATH)
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|secret| !secret.is_empty());
        let accepted = self.receive().is_enabled()
            && secret.is_some_and(|secret| self.book.secret_matches(secret));
        if !accepted {
            return Delivery::NotFound;
        }
        if method != HttpMethod::Post {
            return Delivery::WrongMethod;
        }
        if body.len() > WEBHOOK_BODY_MAX {
            log::warn!(
                "lost a BlueBubbles webhook delivery of {} bytes",
                body.len()
            );
            self.book.lose();
            return Delivery::TooLarge;
        }
        let message = match parse_webhook(body) {
            Ok(WebhookPayload::NewMessage(message)) => message,
            Ok(WebhookPayload::Other) => return Delivery::OtherEvent,
            Err(error) => {
                log::debug!("{error}");
                return Delivery::Invalid;
            }
        };
        if message.is_from_me {
            return Delivery::FromMe;
        }
        match self.book.offer(message) {
            Offer::Queued => Delivery::Queued,
            Offer::Duplicate => Delivery::Duplicate,
            Offer::Full => {
                log::warn!("lost a BlueBubbles webhook delivery: the inbox is full");
                Delivery::Full
            }
        }
    }
}
