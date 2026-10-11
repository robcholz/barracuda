use alloc::{
    boxed::Box,
    collections::VecDeque,
    format,
    rc::Rc,
    string::{String, ToString},
};
use core::{
    cell::Cell,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use barracuda_imessage_gateway_plugin::{
    BinaryBody, BinaryChunk, BinaryStream, ChannelError, ChannelFuture, DeleteMessageRequest,
    EditMessageRequest, MediaKind, MessageChannel, MessageKind, MessageTarget, ReactRequest,
    SendMediaRequest, SendMessageRequest, SendReceipt, SendSessionsRequest, SendStreamRequest,
    SetTypingRequest, TextBody, TextChunk,
};
use barracuda_plugin::api::SharedEntropy;
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    pubsub::{PubSubChannel, Subscriber, WaitResult},
};
use futures_core::Stream;
use futures_lite::{stream, StreamExt};

use crate::media::mime_or_default;
use crate::{KeptMedia, MediaSpool, WebDelivery, WebEvent, WebEventData, WebMedia, MEDIA_PATH};

// Events are shared between the live bus and replay history, so each slot is
// one pointer instead of an inline event.
type Bus<const CAP: usize, const SUBS: usize> =
    PubSubChannel<NoopRawMutex, Rc<WebEvent>, CAP, SUBS, 0>;
type LiveSubscriber<'a, const CAP: usize, const SUBS: usize> =
    Subscriber<'a, NoopRawMutex, Rc<WebEvent>, CAP, SUBS, 0>;

/// Web channel with bounded replay history and subscriber slots.
///
/// History holds events only: a media message is one event naming its URL,
/// while the file stays in storage behind [`WebMedia`].
pub struct Web<const CAP: usize = 32, const SUBS: usize = 4> {
    bus: Bus<CAP, SUBS>,
    history: core::cell::RefCell<VecDeque<Rc<WebEvent>>>,
    next_id: Cell<u64>,
    media: WebMedia,
    spool: Rc<dyn MediaSpool>,
}

impl<const CAP: usize, const SUBS: usize> Web<CAP, SUBS> {
    /// A channel that spools media sent as bytes or a stream to `spool`, and
    /// mints media ids from `entropy`.
    #[must_use]
    pub fn new(spool: Rc<dyn MediaSpool>, entropy: SharedEntropy) -> Self {
        Self {
            bus: PubSubChannel::new(),
            history: core::cell::RefCell::new(VecDeque::new()),
            next_id: Cell::new(1),
            media: WebMedia::new(entropy),
            spool,
        }
    }

    /// The uploads waiting to be sent and the files kept for download.
    #[must_use]
    pub const fn media(&self) -> &WebMedia {
        &self.media
    }

    pub fn subscribe(
        &self,
        conversation_id: impl Into<String>,
    ) -> Result<WebSubscription<'_, CAP, SUBS>, SubscribeError> {
        self.subscribe_from(conversation_id, None)
    }

    /// Subscribe to live events and optionally replay events after `last_event_id`.
    pub fn subscribe_from(
        &self,
        conversation_id: impl Into<String>,
        last_event_id: Option<u64>,
    ) -> Result<WebSubscription<'_, CAP, SUBS>, SubscribeError> {
        let conversation_id = conversation_id.into();
        if conversation_id.trim().is_empty() {
            return Err(SubscribeError::InvalidConversation);
        }
        let live = self
            .bus
            .subscriber()
            .map_err(|_| SubscribeError::MaximumSubscribersReached)?;
        let mut replay = VecDeque::new();
        if let Some(last) = last_event_id {
            let history = self.history.borrow();
            if let Some(oldest) = history.front().map(|event| event.id) {
                let wanted = last.saturating_add(1);
                if wanted < oldest {
                    replay.push_back(WebDelivery::Lagged {
                        missed: oldest.saturating_sub(wanted),
                    });
                }
            }
            replay.extend(
                history
                    .iter()
                    .filter(|event| event.id > last && event.conversation_id == conversation_id)
                    .cloned()
                    .map(WebDelivery::Event),
            );
        }
        Ok(WebSubscription {
            conversation_id,
            replay,
            live,
        })
    }

    fn publish(
        &self,
        target: &MessageTarget,
        data: WebEventData,
    ) -> Result<Rc<WebEvent>, ChannelError> {
        let id = self.next_id.get();
        let next_id = id.checked_add(1).ok_or_else(|| ChannelError::Transport {
            message: String::from("web event sequence exhausted"),
        })?;
        self.next_id.set(next_id);
        let event = Rc::new(WebEvent {
            id,
            conversation_id: target.conversation_id.clone(),
            thread_id: target.thread_id.clone(),
            data,
        });
        {
            let mut history = self.history.borrow_mut();
            if CAP > 0 && history.len() == CAP {
                history.pop_front();
            }
            if CAP > 0 {
                history.push_back(Rc::clone(&event));
            }
        }
        self.bus
            .immediate_publisher()
            .publish_immediate(Rc::clone(&event));
        Ok(event)
    }

    fn message_id(id: u64) -> String {
        format!("web-{id}")
    }
}

impl<const CAP: usize, const SUBS: usize> MessageChannel for Web<CAP, SUBS> {
    fn channel(&self) -> &str {
        "web"
    }

    fn send_message(&self, request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            if matches!(&request.body, TextBody::Complete(text) if text.trim().is_empty()) {
                return Err(ChannelError::InvalidRequest {
                    message: String::from("text must not be empty"),
                });
            }
            let message_id = Self::message_id(self.next_id.get());
            self.publish(
                &request.target,
                WebEventData::MessageStart {
                    message_id: message_id.clone(),
                    reply_to: request.reply_to.clone(),
                    kind: request.kind,
                },
            )?;

            match request.body {
                TextBody::Complete(text) => {
                    self.publish(
                        &request.target,
                        WebEventData::MessageDelta {
                            message_id: message_id.clone(),
                            delta: TextChunk::from(text),
                        },
                    )?;
                }
                TextBody::Stream(mut stream) => {
                    while let Some(chunk) = stream.next().await {
                        match chunk {
                            Ok(delta) if delta.is_empty() => {}
                            Ok(delta) => {
                                self.publish(
                                    &request.target,
                                    WebEventData::MessageDelta {
                                        message_id: message_id.clone(),
                                        delta,
                                    },
                                )?;
                            }
                            Err(error) => {
                                self.publish(
                                    &request.target,
                                    WebEventData::MessageEnd {
                                        message_id: message_id.clone(),
                                        error: Some(stream_error_message(&error)),
                                    },
                                )?;
                                return Err(error.into());
                            }
                        }
                    }
                }
            }
            self.publish(
                &request.target,
                WebEventData::MessageEnd {
                    message_id: message_id.clone(),
                    error: None,
                },
            )?;
            Ok(SendReceipt::new(message_id))
        })
    }

    fn send_stream(&self, mut request: SendStreamRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            let message_id = Self::message_id(self.next_id.get());
            log::info!(
                "IMessage Web opening outbound message `{message_id}` for conversation `{}`",
                request.target.conversation_id
            );
            self.publish(
                &request.target,
                WebEventData::MessageStart {
                    message_id: message_id.clone(),
                    reply_to: request.reply_to.clone(),
                    kind: MessageKind::Reply,
                },
            )?;

            while let Some(event) = request.events.next().await {
                match event {
                    Ok(event) => {
                        match event.event_type.as_str() {
                            "turn_started" | "turn_ended" => log::info!(
                                "IMessage Web forwarding `{}` for `{}` at sequence {}",
                                event.event_type,
                                event.session,
                                event.sequence
                            ),
                            _ => log::debug!(
                                "IMessage Web forwarding `{}` for `{}` at sequence {}",
                                event.event_type,
                                event.session,
                                event.sequence
                            ),
                        }
                        self.publish(
                            &request.target,
                            WebEventData::MessageEvent {
                                message_id: message_id.clone(),
                                event,
                            },
                        )?;
                    }
                    Err(error) => {
                        log::warn!(
                            "IMessage Web outbound message `{message_id}` failed while reading Agent events: {error}"
                        );
                        self.publish(
                            &request.target,
                            WebEventData::MessageEnd {
                                message_id: message_id.clone(),
                                error: Some(stream_error_message(&error)),
                            },
                        )?;
                        return Err(error.into());
                    }
                }
            }

            self.publish(
                &request.target,
                WebEventData::MessageEnd {
                    message_id: message_id.clone(),
                    error: None,
                },
            )?;
            log::info!(
                "IMessage Web completed outbound message `{message_id}` for conversation `{}`",
                request.target.conversation_id
            );
            Ok(SendReceipt::new(message_id))
        })
    }

    fn send_media(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            let SendMediaRequest {
                target,
                body,
                filename,
                mime_type,
                caption,
                reply_to,
                ..
            } = request;
            let mime_type = mime_or_default(mime_type.as_deref(), filename.as_deref());
            // a source is kept as it is; anything else is written to storage
            // first, so no media byte stays in memory or in the history
            let source = match body {
                BinaryBody::Source(source) => source,
                BinaryBody::Bytes(bytes) => {
                    let chunk: BinaryStream = Box::pin(stream::once(Ok(BinaryChunk::from(bytes))));
                    self.spool
                        .spool(kind, mime_type.clone(), filename.clone(), chunk)
                        .await?
                }
                BinaryBody::Stream(body) => {
                    self.spool
                        .spool(kind, mime_type.clone(), filename.clone(), body)
                        .await?
                }
            };
            let size = source.len();
            let id = self.media.keep(KeptMedia {
                source,
                mime_type: mime_type.clone(),
                filename: filename.clone(),
            });
            let message_id = Self::message_id(self.next_id.get());
            log::info!(
                "IMessage Web sending {kind:?} `{message_id}` ({size} bytes) as media `{id}` for conversation `{}`",
                target.conversation_id
            );
            self.publish(
                &target,
                WebEventData::Media {
                    message_id: message_id.clone(),
                    kind,
                    url: format!("{MEDIA_PATH}{id}"),
                    mime_type,
                    filename,
                    size,
                    caption,
                    reply_to,
                },
            )?;
            Ok(SendReceipt::new(message_id))
        })
    }

    fn edit_message(&self, request: EditMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            self.publish(
                &request.target,
                WebEventData::MessageEdit {
                    message_id: request.message_id.clone(),
                    text: request.text,
                },
            )?;
            Ok(SendReceipt::new(request.message_id))
        })
    }

    fn delete_message(&self, request: DeleteMessageRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            self.publish(
                &request.target,
                WebEventData::MessageDelete {
                    message_id: request.message_id,
                },
            )?;
            Ok(())
        })
    }

    fn react(&self, request: ReactRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            self.publish(
                &request.target,
                WebEventData::MessageReaction {
                    message_id: request.message_id,
                    reaction: request.reaction,
                },
            )?;
            Ok(())
        })
    }

    fn send_sessions(&self, request: SendSessionsRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            let json =
                serde_json::to_string(&request).map_err(|error| ChannelError::Transport {
                    message: error.to_string(),
                })?;
            self.publish(&request.target, WebEventData::ConversationSessions { json })?;
            Ok(())
        })
    }

    fn set_typing(&self, request: SetTypingRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            self.publish(
                &request.target,
                WebEventData::ConversationTyping {
                    typing: request.typing,
                },
            )?;
            Ok(())
        })
    }
}

/// Failure to reserve a Web event subscription slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SubscribeError {
    #[error("maximum Web subscribers reached")]
    MaximumSubscribersReached,
    #[error("conversation ID must not be empty")]
    InvalidConversation,
}

/// Replay followed by live events for one SSE connection.
pub struct WebSubscription<'a, const CAP: usize, const SUBS: usize> {
    conversation_id: String,
    replay: VecDeque<WebDelivery>,
    live: LiveSubscriber<'a, CAP, SUBS>,
}

impl<const CAP: usize, const SUBS: usize> WebSubscription<'_, CAP, SUBS> {
    pub async fn next(&mut self) -> Option<WebDelivery> {
        if let Some(delivery) = self.replay.pop_front() {
            return Some(delivery);
        }
        loop {
            match self.live.next_message().await {
                WaitResult::Message(event) if event.conversation_id == self.conversation_id => {
                    return Some(WebDelivery::Event(event));
                }
                WaitResult::Message(_) => {}
                WaitResult::Lagged(missed) => return Some(WebDelivery::Lagged { missed }),
            }
        }
    }
}

impl<const CAP: usize, const SUBS: usize> Stream for WebSubscription<'_, CAP, SUBS> {
    type Item = WebDelivery;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(delivery) = self.replay.pop_front() {
            return Poll::Ready(Some(delivery));
        }
        loop {
            let mut future = self.live.next_message();
            match Pin::new(&mut future).poll(cx) {
                Poll::Ready(WaitResult::Message(event))
                    if event.conversation_id == self.conversation_id =>
                {
                    return Poll::Ready(Some(WebDelivery::Event(event)));
                }
                Poll::Ready(WaitResult::Message(_)) => {}
                Poll::Ready(WaitResult::Lagged(missed)) => {
                    return Poll::Ready(Some(WebDelivery::Lagged { missed }));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

fn stream_error_message(error: &barracuda_imessage_gateway_plugin::StreamError) -> String {
    match error {
        barracuda_imessage_gateway_plugin::StreamError::Failed { message } => message.clone(),
    }
}
