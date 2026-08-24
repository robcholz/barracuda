//! Portable WebSocket endpoint for the [`Web`] channel.
//!
//! The IMessage Web Plugin registers [`WebBridge`] with the global `WebServer`
//! during startup. Socket framing, picoserve, and Platform I/O remain owned by the
//! cross-platform server.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;

use barracuda_webserver_plugin::{
    WebSocketConnection, WebSocketEndpoint, WebSocketFuture, WebSocketMessage,
};
use futures_lite::future;

use crate::{InboundMessage, InboundMessageSink, Web, WebClientFrame, WebDelivery};

enum NextMessage {
    Incoming(Result<WebSocketMessage, barracuda_webserver_plugin::WebSocketClosed>),
    Outgoing(Option<WebDelivery>),
}

/// WebSocket endpoint bridging one Web conversation to the Gateway ingress.
#[derive(Clone)]
pub struct WebBridge<const CAP: usize, const SUBS: usize> {
    web: Rc<Web<CAP, SUBS>>,
    sink: Rc<dyn InboundMessageSink>,
    conversation_id: String,
}

impl<const CAP: usize, const SUBS: usize> WebBridge<CAP, SUBS> {
    /// Builds an endpoint for `conversation_id`.
    #[must_use]
    pub fn new(
        web: Rc<Web<CAP, SUBS>>,
        sink: Rc<dyn InboundMessageSink>,
        conversation_id: impl Into<String>,
    ) -> Self {
        Self {
            web,
            sink,
            conversation_id: conversation_id.into(),
        }
    }
}

impl<const CAP: usize, const SUBS: usize> WebSocketEndpoint for WebBridge<CAP, SUBS> {
    fn connected<'a>(&'a self, connection: WebSocketConnection) -> WebSocketFuture<'a> {
        Box::pin(async move {
            let Ok(mut subscription) = self.web.subscribe(self.conversation_id.clone()) else {
                return;
            };
            let mut next_message: u64 = 1;

            loop {
                let next = future::or(
                    async { NextMessage::Incoming(connection.receive().await) },
                    async { NextMessage::Outgoing(subscription.next().await) },
                )
                .await;
                match next {
                    NextMessage::Incoming(Ok(WebSocketMessage::Text(text))) => {
                        if let Ok(frame) = serde_json::from_str::<WebClientFrame>(&text) {
                            let message_id = format!("web-in-{next_message}");
                            next_message = next_message.saturating_add(1);
                            let message = InboundMessage {
                                conversation_id: self.conversation_id.clone(),
                                message_id,
                                thread_id: None,
                                text: frame.text,
                                reply_to: frame.reply_to,
                            };
                            if self.sink.receive_message(message).await.is_err() {
                                break;
                            }
                        }
                    }
                    NextMessage::Incoming(Ok(WebSocketMessage::Binary(_))) => {}
                    NextMessage::Incoming(Err(_closed)) => break,
                    NextMessage::Outgoing(None) => break,
                    NextMessage::Outgoing(Some(delivery)) => {
                        let Ok(frame) = delivery.to_sse() else {
                            continue;
                        };
                        if connection.send_text(frame).await.is_err() {
                            break;
                        }
                    }
                }
            }
        })
    }
}
