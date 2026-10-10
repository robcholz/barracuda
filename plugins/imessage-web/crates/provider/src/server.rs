//! Portable WebSocket endpoint for the [`Web`] channel.
//!
//! The IMessage Web Plugin registers [`WebBridge`] with the global `WebServer`
//! during registration. Socket framing, picoserve, and Platform I/O remain owned
//! by the cross-platform server.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;

use barracuda_webserver_plugin::{
    WebSocketConnection, WebSocketEndpoint, WebSocketFuture, WebSocketMessage,
};
use futures_lite::future;

use crate::{InboundMessage, InboundMessageSink, Web, WebClientFrame, WebDelivery};

#[allow(clippy::large_enum_variant)] // Keeps the lane-bounded outgoing event inline.
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
            log::info!(
                "IMessage Web client connected for conversation `{}`",
                self.conversation_id
            );
            let mut subscription = match self.web.subscribe(self.conversation_id.clone()) {
                Ok(subscription) => subscription,
                Err(error) => {
                    log::warn!(
                        "IMessage Web client subscription failed for conversation `{}`: {error}",
                        self.conversation_id
                    );
                    return;
                }
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
                        match serde_json::from_str::<WebClientFrame>(&text) {
                            Ok(frame) => {
                                let message_id = format!("web-in-{next_message}");
                                next_message = next_message.saturating_add(1);
                                log::info!(
                                    "IMessage Web received message `{message_id}` for conversation `{}`",
                                    self.conversation_id
                                );
                                let message = InboundMessage {
                                    conversation_id: self.conversation_id.clone(),
                                    message_id: message_id.clone(),
                                    thread_id: None,
                                    text: frame.text,
                                };
                                if let Err(error) = self.sink.receive_message(message).await {
                                    log::warn!(
                                        "IMessage Web failed to deliver message `{message_id}` for conversation `{}`: {error}",
                                        self.conversation_id
                                    );
                                    break;
                                }
                                log::info!(
                                    "IMessage Web accepted message `{message_id}` for conversation `{}`",
                                    self.conversation_id
                                );
                            }
                            Err(error) => {
                                log::warn!(
                                    "IMessage Web rejected malformed WebSocket message for conversation `{}` ({} bytes): {error}",
                                    self.conversation_id,
                                    text.len()
                                );
                            }
                        }
                    }
                    NextMessage::Incoming(Ok(WebSocketMessage::Binary(bytes))) => {
                        log::warn!(
                            "IMessage Web ignored unsupported binary message for conversation `{}` ({} bytes)",
                            self.conversation_id,
                            bytes.len()
                        );
                    }
                    NextMessage::Incoming(Err(error)) => {
                        log::debug!(
                            "IMessage Web client receive ended for conversation `{}`: {error}",
                            self.conversation_id
                        );
                        break;
                    }
                    NextMessage::Outgoing(None) => {
                        log::warn!(
                            "IMessage Web event subscription ended for conversation `{}`",
                            self.conversation_id
                        );
                        break;
                    }
                    NextMessage::Outgoing(Some(delivery)) => {
                        match &delivery {
                            WebDelivery::Event(event) => log::debug!(
                                "IMessage Web sending event {} id={} for conversation `{}`",
                                event.data.event_name(),
                                event.id,
                                event.conversation_id
                            ),
                            WebDelivery::Lagged { missed } => log::warn!(
                                "IMessage Web client missed {missed} event(s) for conversation `{}`",
                                self.conversation_id
                            ),
                        }
                        let frame = match delivery.to_sse() {
                            Ok(frame) => frame,
                            Err(error) => {
                                log::warn!(
                                    "IMessage Web failed to serialize an event for conversation `{}`: {error}",
                                    self.conversation_id
                                );
                                continue;
                            }
                        };
                        if let Err(error) = connection.send_text(frame).await {
                            log::warn!(
                                "IMessage Web failed to send an event for conversation `{}`: {error}",
                                self.conversation_id
                            );
                            break;
                        }
                    }
                }
            }
            log::info!(
                "IMessage Web client disconnected from conversation `{}`",
                self.conversation_id
            );
        })
    }
}
