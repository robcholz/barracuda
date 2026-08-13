use alloc::{collections::BTreeMap, rc::Rc, string::String};

use crate::{
    DeleteMessageRequest, EditMessageRequest, GatewayError, MediaKind, MessageChannel,
    ReactRequest, SendMediaRequest, SendMessageRequest, SendReceipt, SetTypingRequest,
};

/// Registry-backed outbound messaging facade.
#[derive(Default)]
pub struct MessageGateway {
    channels: BTreeMap<String, Rc<dyn MessageChannel>>,
}

impl MessageGateway {
    pub const fn new() -> Self {
        Self {
            channels: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, provider: Rc<dyn MessageChannel>) -> Result<(), GatewayError> {
        let channel = String::from(provider.channel());
        if self.channels.contains_key(&channel) {
            return Err(GatewayError::DuplicateChannel { channel });
        }
        self.channels.insert(channel, provider);
        Ok(())
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

    fn provider(&self, channel: &str) -> Result<&dyn MessageChannel, GatewayError> {
        self.channels
            .get(channel)
            .map(Rc::as_ref)
            .ok_or_else(|| GatewayError::UnknownChannel {
                channel: String::from(channel),
            })
    }
}
