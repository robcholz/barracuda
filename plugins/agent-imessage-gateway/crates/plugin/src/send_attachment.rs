//! The `send_attachment` Agent Tool: a file to the conversation of the
//! session that asks.

use alloc::{
    boxed::Box,
    rc::Rc,
    string::{String, ToString},
};

use barracuda_agent_plugin::tools::{
    BackgroundTool, BackgroundToolFuture, BackgroundToolHandler, ToolContext, ToolError,
    ToolInvokeError, ToolOutput, ToolSpec,
};
use barracuda_imessage_gateway_plugin::{
    GatewaySendMediaRequest, IMessageGateway, MediaKind, extension_mime,
};
use barracuda_plugin::manager::PluginStorage;
use serde::Deserialize;

use crate::bridge::SessionConversations;
use crate::tool_output;

/// The refusal of a session that talks in no channel conversation.
const NO_CONVERSATION: &str =
    "no_conversation: this session does not talk in a channel conversation";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendAttachmentArgs {
    path: String,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    kind: Option<MediaKind>,
}

/// Sends a file through the Gateway to the conversation the calling session
/// is bound to, and completes with the Gateway's answer.
pub(crate) struct SendAttachmentTool<Storage> {
    gateway: Rc<IMessageGateway>,
    conversations: SessionConversations<Storage>,
}

impl<Storage> SendAttachmentTool<Storage> {
    pub(crate) const fn new(
        gateway: Rc<IMessageGateway>,
        conversations: SessionConversations<Storage>,
    ) -> Self {
        Self {
            gateway,
            conversations,
        }
    }

    /// The Gateway request that sends `args` to the conversation of `context`'s
    /// session, or the refusal when it has none.
    pub(crate) async fn request(
        &self,
        context: ToolContext,
        args: SendAttachmentArgs,
    ) -> Result<GatewaySendMediaRequest, ToolInvokeError> {
        let target = self
            .conversations
            .of(&context.session.to_wire())
            .await
            .ok_or_else(|| ToolError::InvokeRejected(String::from(NO_CONVERSATION)))?;
        let kind = args.kind.unwrap_or_else(|| kind_of(&args.path));
        Ok(GatewaySendMediaRequest::File {
            channel: target.route.channel,
            conversation_id: target.route.conversation_id,
            thread_id: target.route.thread_id,
            kind,
            path: args.path,
            filename: None,
            mime_type: None,
            caption: args.caption,
            reply_to: target.reply_to,
        })
    }
}

/// How a channel shows the file at `path`, from the type its extension names.
fn kind_of(path: &str) -> MediaKind {
    match extension_mime(path).and_then(|mime| mime.split_once('/')) {
        Some(("image", _subtype)) => MediaKind::Image,
        Some(("audio", _subtype)) => MediaKind::Audio,
        Some(("video", _subtype)) => MediaKind::Video,
        _ => MediaKind::File,
    }
}

/// Sends one file and answers with the Gateway's receipt or business error.
pub(crate) async fn deliver(
    gateway: &IMessageGateway,
    request: GatewaySendMediaRequest,
) -> Result<ToolOutput, ToolInvokeError> {
    tool_output(gateway.send_media(request).await)
}

impl<Storage> ToolSpec for SendAttachmentTool<Storage> {
    barracuda_agent_plugin::tools::tool_metadata!("send_attachment");
}

impl<Storage> BackgroundToolHandler for SendAttachmentTool<Storage>
where
    Storage: PluginStorage,
{
    type Args = SendAttachmentArgs;

    fn invoke<'a>(&'a self, context: ToolContext, args: Self::Args) -> BackgroundToolFuture<'a> {
        Box::pin(async move {
            let accepted = ToolOutput::new(
                serde_json::json!({ "sending": args.path }).to_string(),
                true,
            );
            let request = self.request(context, args).await?;
            let gateway = Rc::clone(&self.gateway);
            Ok(BackgroundTool::new(
                accepted,
                Box::pin(async move { deliver(&gateway, request).await }),
            ))
        })
    }
}
