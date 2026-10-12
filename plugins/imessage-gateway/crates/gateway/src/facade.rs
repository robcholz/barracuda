use alloc::{collections::BTreeMap, rc::Rc, string::String};
use core::cell::RefCell;

use crate::{
    DeleteMessageRequest, EditMessageRequest, GatewayError, MediaKind, MessageChannel,
    ReactRequest, SendMediaRequest, SendMessageRequest, SendReceipt, SendSessionsRequest,
    SendStreamRequest, SetTypingRequest,
};

/// Registry-backed outbound messaging facade.
#[derive(Clone, Default)]
pub struct MessageGateway {
    channels: Rc<RefCell<BTreeMap<String, Rc<dyn MessageChannel>>>>,
}

impl MessageGateway {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &self,
        provider: Rc<dyn MessageChannel>,
    ) -> Result<MessageChannelRegistration, GatewayError> {
        let channel = String::from(provider.channel());
        let mut channels = self.channels.borrow_mut();
        if channels.contains_key(&channel) {
            return Err(GatewayError::DuplicateChannel { channel });
        }
        channels.insert(channel.clone(), Rc::clone(&provider));
        drop(channels);
        Ok(MessageChannelRegistration {
            channels: Rc::clone(&self.channels),
            channel,
            provider,
        })
    }

    /// Whether a provider is registered for `channel` now.
    pub fn has_channel(&self, channel: &str) -> bool {
        self.channels.borrow().contains_key(channel)
    }

    pub async fn send_message(
        &self,
        request: SendMessageRequest,
    ) -> Result<SendReceipt, GatewayError> {
        let channel = request.target.channel.clone();
        self.provider(&channel)?
            .send_message(request)
            .await
            .map_err(|source| GatewayError::Channel { channel, source })
    }

    /// Sends one ordered primary-text stream with optional extra-content frames.
    pub async fn send_stream(
        &self,
        request: SendStreamRequest,
    ) -> Result<SendReceipt, GatewayError> {
        let channel = request.target.channel.clone();
        self.provider(&channel)?
            .send_stream(request)
            .await
            .map_err(|source| GatewayError::Channel { channel, source })
    }

    pub async fn send_file(&self, request: SendMediaRequest) -> Result<SendReceipt, GatewayError> {
        self.send_media(MediaKind::File, request).await
    }

    pub async fn send_image(&self, request: SendMediaRequest) -> Result<SendReceipt, GatewayError> {
        self.send_media(MediaKind::Image, request).await
    }

    pub async fn send_audio(&self, request: SendMediaRequest) -> Result<SendReceipt, GatewayError> {
        self.send_media(MediaKind::Audio, request).await
    }

    pub async fn send_video(&self, request: SendMediaRequest) -> Result<SendReceipt, GatewayError> {
        self.send_media(MediaKind::Video, request).await
    }

    pub async fn edit_message(
        &self,
        request: EditMessageRequest,
    ) -> Result<SendReceipt, GatewayError> {
        let channel = request.target.channel.clone();
        self.provider(&channel)?
            .edit_message(request)
            .await
            .map_err(|source| GatewayError::Channel { channel, source })
    }

    pub async fn delete_message(&self, request: DeleteMessageRequest) -> Result<(), GatewayError> {
        let channel = request.target.channel.clone();
        self.provider(&channel)?
            .delete_message(request)
            .await
            .map_err(|source| GatewayError::Channel { channel, source })
    }

    pub async fn react(&self, request: ReactRequest) -> Result<(), GatewayError> {
        let channel = request.target.channel.clone();
        self.provider(&channel)?
            .react(request)
            .await
            .map_err(|source| GatewayError::Channel { channel, source })
    }

    /// Shows a conversation's sessions after a session command.
    pub async fn send_sessions(&self, request: SendSessionsRequest) -> Result<(), GatewayError> {
        let channel = request.target.channel.clone();
        self.provider(&channel)?
            .send_sessions(request)
            .await
            .map_err(|source| GatewayError::Channel { channel, source })
    }

    pub async fn set_typing(&self, request: SetTypingRequest) -> Result<(), GatewayError> {
        let channel = request.target.channel.clone();
        self.provider(&channel)?
            .set_typing(request)
            .await
            .map_err(|source| GatewayError::Channel { channel, source })
    }

    async fn send_media(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> Result<SendReceipt, GatewayError> {
        let channel = request.target.channel.clone();
        self.provider(&channel)?
            .send_media(kind, request)
            .await
            .map_err(|source| GatewayError::Channel { channel, source })
    }

    fn provider(&self, channel: &str) -> Result<Rc<dyn MessageChannel>, GatewayError> {
        self.channels
            .borrow()
            .get(channel)
            .cloned()
            .ok_or_else(|| GatewayError::UnknownChannel {
                channel: String::from(channel),
            })
    }
}

/// Guard that unregisters one message channel when dropped.
#[must_use = "dropping the registration unregisters the message channel"]
pub struct MessageChannelRegistration {
    channels: Rc<RefCell<BTreeMap<String, Rc<dyn MessageChannel>>>>,
    channel: String,
    provider: Rc<dyn MessageChannel>,
}

impl Drop for MessageChannelRegistration {
    fn drop(&mut self) {
        let mut channels = self.channels.borrow_mut();
        let owns_registration = channels
            .get(&self.channel)
            .is_some_and(|registered| Rc::ptr_eq(registered, &self.provider));
        if owns_registration {
            channels.remove(&self.channel);
        }
    }
}
