//! Inbound attachments stored under `/workspace/cache/gateway/<channel>/`,
//! files opened for sending, downloads into storage, and the `file` command
//! of `gateway.send_media`, on a real Gateway beside Workflow, HTTP, and
//! WebServer.
//!
//! Every test runs in a process of its own: the memory filesystem is
//! process-wide, and each test inspects the cache directory it writes.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    missing_docs
)]

use std::cell::RefCell;
use std::rc::Rc;

use barracuda_http_plugin::HttpPlugin;
use barracuda_imessage_gateway_plugin::{
    Attachment, AttachmentMeta, BinaryBody, BinaryChunk, BinaryStream, ChannelFuture,
    DownloadRequest, GatewayOperationError, GatewaySendMediaRequest, GatewaySendMediaResponse,
    IMessageGateway, IMessageGatewayPlugin, InboundDownloadError, MediaKind, MessageChannel,
    MessageChannelRegistration, SendMediaRequest, SendMessageRequest, SendReceipt, StoreError,
    StreamError,
};
use barracuda_platform_test::{
    install_global_memory_vfs, loopback_network, memory_partition, ScriptStep, ScriptedServer,
};
use barracuda_plugin::api::{BoardInfo, Hardware, PlatformInfo, PluginContext, TargetIdentity};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginFilesystem, PluginManager, PluginRegisterContext,
    PluginRequirements, PluginResult, PluginStorage,
};
use barracuda_vfs::{FsError, ScopedVfs};
use barracuda_webserver_plugin::WebServerPlugin;
use barracuda_workflow_plugin::{workflow_action_schema, WorkflowActionSchema, WorkflowPlugin};
use embassy_futures::select::{select3, Either3};
use futures_lite::{future::block_on, stream, StreamExt};
use rusty_fork::rusty_fork_test;
use serde_json::{json, Value};

const SEND_MEDIA: WorkflowActionSchema = workflow_action_schema!("gateway.send_media");
/// Port of the scripted media server on the loopback network.
const PORT: u16 = 9000;
const ORIGIN: &str = "http://10.0.0.1:9000";

type Captured = Rc<RefCell<Option<(Rc<IMessageGateway>, ScopedVfs)>>>;

/// Another Plugin's view of the shared filesystem, and the Gateway.
struct Probe(Captured);

impl PluginDeclaration for Probe {
    const ID: &'static str = "attachment-probe";
    const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway"];
}

impl Plugin for Probe {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage: PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        self.0
            .replace(Some((gateway, context.filesystem()?.clone())));
        Ok(())
    }
}

/// Runs `test` on a real Gateway with a scripted media server answering
/// `responses` on a loopback network.
fn bench(
    responses: Vec<ScriptStep>,
    test: impl AsyncFnOnce(&IMessageGateway, &ScopedVfs, &ScriptedServer),
) {
    let network = loopback_network();
    let stack = network.stack();
    let server = ScriptedServer::new(responses);
    block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition = block_on(memory_partition(64 * 1024)).expect("test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
    let identity = TargetIdentity::new(
        PlatformInfo::new("test", "test", "test-arch", "hosted"),
        BoardInfo::new("test-board", Hardware::new("test-chip")),
    );
    let mut context = PluginContext::new(identity, stack);
    let captured: Captured = Rc::new(RefCell::new(None));
    manager
        .register(WorkflowPlugin::new(&mut context))
        .expect("register Workflow");
    manager
        .register(HttpPlugin::new(&mut context))
        .expect("register HTTP");
    manager
        .register(WebServerPlugin::new(&mut context))
        .expect("register WebServer");
    manager
        .register(IMessageGatewayPlugin::new(&mut context))
        .expect("register Gateway");
    manager
        .register(Probe(Rc::clone(&captured)))
        .expect("register probe");
    let (gateway, filesystem) = captured.take().expect("probe captured the Gateway");
    let outcome = block_on(select3(
        network.run(),
        server.run::<4>(stack, PORT),
        test(&gateway, &filesystem, &server),
    ));
    assert!(matches!(outcome, Either3::Third(())), "the bench stopped");
}

fn chunks(parts: &[&[u8]]) -> BinaryStream {
    let parts: Vec<_> = parts
        .iter()
        .map(|part| Ok(BinaryChunk::from(part.to_vec())))
        .collect();
    Box::pin(stream::iter(parts))
}

fn meta(kind: MediaKind, mime_type: Option<&str>, filename: Option<&str>) -> AttachmentMeta {
    AttachmentMeta {
        kind,
        mime_type: mime_type.map(String::from),
        filename: filename.map(String::from),
    }
}

async fn names(filesystem: &ScopedVfs, channel: &str) -> Vec<String> {
    match filesystem
        .list_dir(&format!("/workspace/cache/gateway/{channel}"))
        .await
    {
        Ok(mut names) => {
            names.sort();
            names
        }
        Err(FsError::NotFound) => Vec::new(),
        Err(error) => panic!("list attachments: {error}"),
    }
}

async fn read_all(mut stream: BinaryStream) -> Result<Vec<u8>, StreamError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk?);
    }
    Ok(bytes)
}

/// A response head declaring `length` body bytes, followed by none.
fn declared(status: u16, content_type: &str, length: u64) -> ScriptStep {
    ScriptStep::Response {
        bytes: format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
        )
        .into_bytes(),
        max_read: usize::MAX,
        pending_after: false,
    }
}

/// What [`Recorder`] saw of one `send_media`.
#[derive(Debug, Eq, PartialEq)]
struct Sent {
    kind: MediaKind,
    conversation_id: String,
    filename: Option<String>,
    mime_type: Option<String>,
    caption: Option<String>,
    size: Option<u64>,
    /// The source read twice, as a provider that digests first does.
    reads: [Vec<u8>; 2],
}

struct Recorder(Rc<RefCell<Vec<Sent>>>);

impl MessageChannel for Recorder {
    fn channel(&self) -> &str {
        "files"
    }

    fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async { Ok(SendReceipt::new("text")) })
    }

    fn send_media(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            let BinaryBody::Source(source) = request.body else {
                panic!("a file is sent as a source");
            };
            let first = read_all(source.open().await?).await?;
            let second = read_all(source.open().await?).await?;
            assert_eq!(u64::try_from(first.len()).ok(), Some(source.len()));
            self.0.borrow_mut().push(Sent {
                kind,
                conversation_id: request.target.conversation_id,
                filename: request.filename,
                mime_type: request.mime_type,
                caption: request.caption,
                size: request.size,
                reads: [first, second],
            });
            Ok(SendReceipt::new("sent-1"))
        })
    }
}

fn recorder(gateway: &IMessageGateway) -> (MessageChannelRegistration, Rc<RefCell<Vec<Sent>>>) {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let registration = gateway
        .register(Rc::new(Recorder(Rc::clone(&sent))))
        .expect("register the recording channel");
    (registration, sent)
}

/// Runs one `gateway.send_media` request through its schemas, as the
/// Workflow Action does, and returns the response JSON.
async fn send_media(gateway: &IMessageGateway, request: Value) -> Value {
    SEND_MEDIA
        .request()
        .validate(&request)
        .expect("the request matches the Action schema");
    let request: GatewaySendMediaRequest =
        serde_json::from_value(request).expect("decode the request");
    let response = match gateway.send_media(request).await {
        Ok(response) => serde_json::to_value(response).expect("encode the response"),
        Err(error) => json!({ "error": error }),
    };
    SEND_MEDIA
        .response()
        .validate(&response)
        .expect("the response matches the Action schema");
    response
}

rusty_fork_test! {
    #[test]
    fn an_inbound_attachment_is_written_beside_its_name_then_renamed_into_place() {
        bench(Vec::new(), async |gateway, filesystem, _server| {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let observed = Rc::clone(&seen);
            let probe = filesystem.clone();
            // Lists the directory between the two chunks, while the file is
            // still being written.
            let body: BinaryStream = Box::pin(stream::unfold(0, move |step| {
                let observed = Rc::clone(&observed);
                let probe = probe.clone();
                async move {
                    match step {
                        0 => Some((Ok(BinaryChunk::from(b"JPEG".to_vec())), 1)),
                        1 => {
                            observed.replace(names(&probe, "telegram").await);
                            Some((Ok(BinaryChunk::from(b"DATA".to_vec())), 2))
                        }
                        _ => None,
                    }
                }
            }));

            let attachment = gateway
                .store_inbound(
                    "telegram",
                    meta(MediaKind::Image, Some("image/jpeg"), Some("photo.jpg")),
                    body,
                )
                .await
                .expect("store the attachment");

            let directory = "/workspace/cache/gateway/telegram/";
            let name = attachment
                .path
                .strip_prefix(directory)
                .expect("stored in the channel's directory");
            assert!(name.ends_with(".jpg"), "{name}");
            assert_eq!(
                attachment,
                Attachment {
                    kind: MediaKind::Image,
                    path: attachment.path.clone(),
                    mime_type: "image/jpeg".into(),
                    filename: Some("photo.jpg".into()),
                    size: 8,
                }
            );
            assert_eq!(*seen.borrow(), vec![format!(".{name}.part")]);
            assert_eq!(names(filesystem, "telegram").await, vec![name.to_owned()]);
            assert_eq!(
                filesystem.read(&attachment.path).await.expect("read back"),
                b"JPEGDATA".to_vec()
            );

            let second = gateway
                .store_inbound("telegram", meta(MediaKind::File, None, None), chunks(&[b"x"]))
                .await
                .expect("store a second attachment");
            assert_ne!(second.path, attachment.path);
            assert!(second.path.ends_with(".bin"), "{}", second.path);
            assert_eq!(second.mime_type, "application/octet-stream");
            assert_eq!(second.filename, None);
        });
    }
}

rusty_fork_test! {
    #[test]
    fn an_attachment_over_the_limit_or_with_a_failed_stream_leaves_no_file() {
        bench(Vec::new(), async |gateway, filesystem, _server| {
            let limit = gateway.attachment_max_bytes();
            let block = vec![7_u8; 64 * 1024];
            let blocks = usize::try_from(limit / 65_536 + 1).expect("block count");
            let oversized: BinaryStream = Box::pin(stream::iter(
                (0..blocks).map(move |_index| Ok(BinaryChunk::from(block.clone()))),
            ));

            let too_large = gateway
                .store_inbound("web", meta(MediaKind::Video, Some("video/mp4"), None), oversized)
                .await;

            assert_eq!(too_large, Err(StoreError::TooLarge { limit }));
            assert_eq!(names(filesystem, "web").await, Vec::<String>::new());

            let broken: BinaryStream = Box::pin(stream::iter([
                Ok(BinaryChunk::from(b"part".to_vec())),
                Err(StreamError::failed("connection reset")),
            ]));
            let failed = gateway
                .store_inbound("web", meta(MediaKind::Audio, Some("audio/ogg"), None), broken)
                .await;

            assert_eq!(
                failed,
                Err(StoreError::Stream(StreamError::failed("connection reset")))
            );
            assert_eq!(names(filesystem, "web").await, Vec::<String>::new());
        });
    }
}

rusty_fork_test! {
    #[test]
    fn a_channel_name_is_one_safe_directory() {
        bench(Vec::new(), async |gateway, filesystem, _server| {
            let attachment = gateway
                .store_inbound("../../data", meta(MediaKind::File, Some("text/plain"), None), chunks(&[b"hi"]))
                .await
                .expect("store the attachment");

            assert!(
                attachment.path.starts_with("/workspace/cache/gateway/______data/"),
                "{}",
                attachment.path
            );
            assert!(attachment.path.ends_with(".txt"));
            assert_eq!(filesystem.read(&attachment.path).await.ok(), Some(b"hi".to_vec()));
        });
    }
}

rusty_fork_test! {
    #[test]
    fn an_opened_source_has_its_length_and_reads_again_from_the_start() {
        bench(Vec::new(), async |gateway, filesystem, _server| {
            let content: Vec<u8> = (0..5000_u32).map(|index| index.to_le_bytes()[0]).collect();
            filesystem
                .write("/workspace/media/report.pdf", &content)
                .await
                .expect("write a file");

            let source = gateway
                .open_source("/workspace/media/report.pdf")
                .await
                .expect("open the file");

            assert_eq!(source.len(), 5000);
            let first = read_all(source.open().await.expect("open a stream")).await;
            let second = read_all(source.open().await.expect("open again")).await;
            assert_eq!(first.as_ref(), Ok(&content));
            assert_eq!(second, Ok(content));
            assert!(matches!(
                gateway.open_source("/workspace/media/missing.pdf").await,
                Err(StoreError::Filesystem(FsError::NotFound))
            ));
            assert!(matches!(
                gateway.open_source("/workspace/media").await,
                Err(StoreError::Filesystem(FsError::IsDirectory))
            ));
        });
    }
}

rusty_fork_test! {
    #[test]
    fn a_download_is_stored_with_the_response_content_type() {
        let steps = vec![
            ScriptStep::response(200, "image/png; charset=binary", b"PNGDATA", 3),
            ScriptStep::response(200, "application/octet-stream", b"OGG", usize::MAX),
            ScriptStep::json(404, r#"{"error":"gone"}"#),
            declared(200, "video/mp4", 1 << 40),
        ];
        bench(steps, async |gateway, filesystem, server| {
            let url = format!("{ORIGIN}/file/a");
            let headers = [("Authorization", "Bearer secret")];
            let image = gateway
                .download_inbound(
                    "telegram",
                    DownloadRequest::get(&url).with_headers(&headers),
                    meta(MediaKind::Image, None, Some("a.png")),
                )
                .await
                .expect("download the image");

            assert!(image.path.ends_with(".png"), "{}", image.path);
            assert_eq!(image.mime_type, "image/png");
            assert_eq!(image.size, 7);
            assert_eq!(image.filename.as_deref(), Some("a.png"));
            assert_eq!(filesystem.read(&image.path).await.ok(), Some(b"PNGDATA".to_vec()));
            let request = &server.requests()[0];
            assert!(request.starts_with("GET /file/a HTTP/1.1\r\n"), "{request}");
            assert!(request.contains("Authorization: Bearer secret\r\n"));

            let voice = gateway
                .download_inbound(
                    "telegram",
                    DownloadRequest::get(&url),
                    meta(MediaKind::Audio, Some("audio/ogg"), None),
                )
                .await
                .expect("download the voice note");
            assert_eq!(voice.mime_type, "audio/ogg");
            assert!(voice.path.ends_with(".ogg"), "{}", voice.path);

            let gone = gateway
                .download_inbound("telegram", DownloadRequest::get(&url), meta(MediaKind::File, None, None))
                .await;
            assert!(matches!(gone, Err(InboundDownloadError::Status(404))), "{gone:?}");

            let huge = gateway
                .download_inbound("telegram", DownloadRequest::get(&url), meta(MediaKind::Video, None, None))
                .await;
            assert!(
                matches!(huge, Err(InboundDownloadError::Store(StoreError::TooLarge { .. }))),
                "{huge:?}"
            );
            let mut stored = names(filesystem, "telegram").await;
            stored.retain(|name| name.starts_with('.'));
            assert_eq!(stored, Vec::<String>::new(), "no partial file is left");
            assert_eq!(names(filesystem, "telegram").await.len(), 2);
        });
    }
}

rusty_fork_test! {
    #[test]
    fn the_file_command_sends_a_path_with_defaults_from_its_name() {
        bench(Vec::new(), async |gateway, filesystem, _server| {
            let (_registration, sent) = recorder(gateway);
            filesystem
                .write("/workspace/cache/out/chart.png", b"PNG!")
                .await
                .expect("write a file");

            let response = send_media(
                gateway,
                json!({
                    "action": "file",
                    "channel": "files",
                    "conversation_id": "chat-1",
                    "kind": "image",
                    "path": "/workspace/cache/out/chart.png",
                    "caption": "today"
                }),
            )
            .await;

            assert_eq!(response, json!({ "message_id": "sent-1" }));
            assert_eq!(
                sent.borrow().as_slice(),
                [Sent {
                    kind: MediaKind::Image,
                    conversation_id: "chat-1".into(),
                    filename: Some("chart.png".into()),
                    mime_type: Some("image/png".into()),
                    caption: Some("today".into()),
                    size: Some(4),
                    reads: [b"PNG!".to_vec(), b"PNG!".to_vec()],
                }]
            );

            let named = send_media(
                gateway,
                json!({
                    "action": "file",
                    "channel": "files",
                    "conversation_id": "chat-1",
                    "kind": "file",
                    "path": "/workspace/cache/out/chart.png",
                    "filename": "plot",
                    "mime_type": "application/x-plot"
                }),
            )
            .await;
            assert_eq!(named, json!({ "message_id": "sent-1" }));
            let last = sent.borrow_mut().pop().expect("second send");
            assert_eq!(last.filename.as_deref(), Some("plot"));
            assert_eq!(last.mime_type.as_deref(), Some("application/x-plot"));
        });
    }
}

rusty_fork_test! {
    #[test]
    fn the_file_command_reports_missing_files_unknown_channels_and_oversized_files() {
        bench(Vec::new(), async |gateway, filesystem, _server| {
            let (_registration, sent) = recorder(gateway);
            let file = |channel: &str, path: &str| {
                json!({
                    "action": "file",
                    "channel": channel,
                    "conversation_id": "chat-1",
                    "kind": "file",
                    "path": path
                })
            };

            assert_eq!(
                send_media(gateway, file("files", "/workspace/cache/none.bin")).await,
                json!({ "error": "not_found" })
            );
            assert_eq!(
                send_media(gateway, file("nobody", "/workspace/cache/none.bin")).await,
                json!({ "error": "unknown_channel" })
            );
            let limit = usize::try_from(gateway.attachment_max_bytes()).expect("limit");
            filesystem
                .write("/workspace/cache/big.bin", &vec![0; limit + 1])
                .await
                .expect("write an oversized file");
            assert_eq!(
                send_media(gateway, file("files", "/workspace/cache/big.bin")).await,
                json!({ "error": "too_large" })
            );
            assert!(sent.borrow().is_empty());

            // The stream commands answer as before.
            assert_eq!(
                send_media(
                    gateway,
                    json!({
                        "action": "start",
                        "stream_id": "s1",
                        "sequence": 0,
                        "channel": "nobody",
                        "conversation_id": "chat-1",
                        "kind": "image"
                    }),
                )
                .await,
                json!({ "error": "unknown_channel" })
            );
            let accepted = gateway
                .send_media(GatewaySendMediaRequest::Start {
                    stream_id: "s2".into(),
                    sequence: 0,
                    channel: "files".into(),
                    conversation_id: "chat-1".into(),
                    thread_id: None,
                    kind: MediaKind::File,
                    filename: None,
                    mime_type: None,
                    caption: None,
                    reply_to: None,
                })
                .await;
            assert!(matches!(
                accepted,
                Ok(GatewaySendMediaResponse::Accepted(accepted)) if accepted.accepted_sequence == 0
            ));
            let error: Result<GatewaySendMediaResponse, GatewayOperationError> = gateway
                .send_media(GatewaySendMediaRequest::Finish {
                    stream_id: "s3".into(),
                    sequence: 1,
                })
                .await;
            assert!(matches!(error, Err(GatewayOperationError::UnknownStream)));
        });
    }
}
