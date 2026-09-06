use alloc::{boxed::Box, collections::VecDeque, format, string::String};
use core::{
    cell::Cell,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    pubsub::{PubSubChannel, Subscriber, WaitResult},
};
use futures_core::Stream;
use futures_lite::StreamExt;
use gateway::{
    BinaryBody, ChannelError, ChannelFuture, DeleteMessageRequest, EditMessageRequest, MediaKind,
    MessageChannel, MessageKind, MessageTarget, ReactRequest, SendMediaRequest, SendMessageRequest,
    SendReceipt, SendStreamRequest, SetTypingRequest, TextBody,
};

use crate::{MediaPhase, WebDelivery, WebEvent, WebEventData};

type Bus<const CAP: usize, const SUBS: usize> = PubSubChannel<NoopRawMutex, WebEvent, CAP, SUBS, 0>;
type LiveSubscriber<'a, const CAP: usize, const SUBS: usize> =
    Subscriber<'a, NoopRawMutex, WebEvent, CAP, SUBS, 0>;

/// Web channel with bounded replay history and subscriber slots.
pub struct Web<const CAP: usize = 32, const SUBS: usize = 4> {
    bus: Bus<CAP, SUBS>,
    history: core::cell::RefCell<VecDeque<WebEvent>>,
    next_id: Cell<u64>,
}

impl<const CAP: usize, const SUBS: usize> Web<CAP, SUBS> {
    pub const fn new() -> Self {
        Self {
            bus: PubSubChannel::new(),
            history: core::cell::RefCell::new(VecDeque::new()),
            next_id: Cell::new(1),
        }
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
    ) -> Result<WebEvent, ChannelError> {
        let id = self.next_id.get();
        let next_id = id.checked_add(1).ok_or_else(|| ChannelError::Transport {
            message: String::from("web event sequence exhausted"),
        })?;
        self.next_id.set(next_id);
        let event = WebEvent {
            id,
            conversation_id: target.conversation_id.clone(),
            thread_id: target.thread_id.clone(),
            data,
        };
        {
            let mut history = self.history.borrow_mut();
            if CAP > 0 && history.len() == CAP {
                history.pop_front();
            }
            if CAP > 0 {
                history.push_back(event.clone());
            }
        }
        self.bus
            .immediate_publisher()
            .publish_immediate(event.clone());
        Ok(event)
    }

    fn message_id(id: u64) -> String {
        format!("web-{id}")
    }
}

impl<const CAP: usize, const SUBS: usize> Default for Web<CAP, SUBS> {
    fn default() -> Self {
        Self::new()
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
                            delta: text,
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
                                        delta: delta.into_string(),
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
                        self.publish(
                            &request.target,
                            WebEventData::MessageEvent {
                                message_id: message_id.clone(),
                                event,
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

    fn send_media(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            let message_id = Self::message_id(self.next_id.get());
            self.publish(
                &request.target,
                WebEventData::Media {
                    message_id: message_id.clone(),
                    kind,
                    phase: MediaPhase::Start {
                        filename: request.filename,
                        mime_type: request.mime_type,
                        caption: request.caption,
                        reply_to: request.reply_to,
                    },
                },
            )?;
            match request.body {
                BinaryBody::Bytes(bytes) => {
                    self.publish(
                        &request.target,
                        WebEventData::Media {
                            message_id: message_id.clone(),
                            kind,
                            phase: MediaPhase::Delta { bytes },
                        },
                    )?;
                }
                BinaryBody::Stream(mut stream) => {
                    while let Some(chunk) = stream.next().await {
                        match chunk {
                            Ok(bytes) if bytes.is_empty() => {}
                            Ok(bytes) => {
                                self.publish(
                                    &request.target,
                                    WebEventData::Media {
                                        message_id: message_id.clone(),
                                        kind,
                                        phase: MediaPhase::Delta {
                                            bytes: bytes.into_vec(),
                                        },
                                    },
                                )?;
                            }
                            Err(error) => {
                                self.publish(
                                    &request.target,
                                    WebEventData::Media {
                                        message_id: message_id.clone(),
                                        kind,
                                        phase: MediaPhase::End {
                                            error: Some(stream_error_message(&error)),
                                        },
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
                WebEventData::Media {
                    message_id: message_id.clone(),
                    kind,
                    phase: MediaPhase::End { error: None },
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

fn stream_error_message(error: &gateway::StreamError) -> String {
    match error {
        gateway::StreamError::Failed { message } => message.clone(),
    }
}
