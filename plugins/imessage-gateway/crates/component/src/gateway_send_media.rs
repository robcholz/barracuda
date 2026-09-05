use alloc::{boxed::Box, collections::BTreeMap, rc::Rc, string::String};
use core::{
    cell::{Cell, RefCell},
    fmt,
};

use async_channel::{Receiver, Sender, TrySendError};
use barracuda_event_router::{
    json_schema, Event, EventEmitter, JsonHandler, JsonPayload, JsonRef, JsonRpcSchema, JsonSchema,
    JsonWriter, RpcError,
};
use futures_lite::stream;
use gateway::{
    BinaryBody, BinaryChunk, MediaKind, MessageGateway, MessageTarget, SendMediaRequest,
    StreamError,
};
use serde::Deserialize;

use crate::component::STREAM_WORKERS;
use crate::gateway_send::map_gateway_error;
use crate::json::{
    encoded_json_len, event_input_capacity, valid_required, valid_stream_id, write_encoded_json,
    write_json_string, AckResponse, EncodedJson, ErrorResponse, GatewayJsonError, FRAME_CAPACITY,
};

const CHUNK_QUEUE_CAPACITY: usize = 2;

/// Applies one bounded command to an outbound media stream.
pub struct GatewaySendMedia;

impl JsonRpcSchema for GatewaySendMedia {
    const ADDRESS: &'static str = "gateway.send_media";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("send_media", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("send_media", response);
    const MAX_REQUEST_BYTES: usize = FRAME_CAPACITY;
    const MAX_RESPONSE_BYTES: usize = 128;
}

/// Terminal outcome for one accepted outbound media stream.
pub struct GatewaySendMediaFinished;

impl Event for GatewaySendMediaFinished {
    const ID: &'static str = "gateway.send_media.finished";
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum GatewayMediaKind {
    File,
    Image,
    Audio,
    Video,
}

impl From<GatewayMediaKind> for MediaKind {
    fn from(kind: GatewayMediaKind) -> Self {
        match kind {
            GatewayMediaKind::File => Self::File,
            GatewayMediaKind::Image => Self::Image,
            GatewayMediaKind::Audio => Self::Audio,
            GatewayMediaKind::Video => Self::Video,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum MediaRequest<'a> {
    Start {
        #[serde(borrow)]
        stream_id: &'a str,
        sequence: u32,
        #[serde(borrow)]
        channel: &'a str,
        #[serde(borrow)]
        conversation_id: &'a str,
        #[serde(default, borrow)]
        thread_id: Option<&'a str>,
        kind: GatewayMediaKind,
        #[serde(default, borrow)]
        filename: Option<&'a str>,
        #[serde(default, borrow)]
        mime_type: Option<&'a str>,
        #[serde(default, borrow)]
        caption: Option<&'a str>,
        #[serde(default, borrow)]
        reply_to: Option<&'a str>,
    },
    Chunk {
        #[serde(borrow)]
        stream_id: &'a str,
        sequence: u32,
        #[serde(borrow)]
        content_base64: &'a str,
    },
    Finish {
        #[serde(borrow)]
        stream_id: &'a str,
        sequence: u32,
    },
}

enum MediaCommand {
    Chunk(JsonRef),
    Finish(u32),
}

struct MediaSession {
    next_sequence: Cell<u32>,
    commands: Sender<MediaCommand>,
}

pub(crate) struct MediaJob {
    stream_id: String,
    terminal_sequence: Rc<RefCell<Option<u32>>>,
    kind: MediaKind,
    request: SendMediaRequest,
}

#[derive(Default)]
pub(crate) struct MediaSessions {
    entries: RefCell<BTreeMap<String, Rc<MediaSession>>>,
}

impl MediaSessions {
    pub(crate) fn clear(&self) {
        self.entries.borrow_mut().clear();
    }

    fn remove(&self, stream_id: &str) {
        self.entries.borrow_mut().remove(stream_id);
    }

    fn prepare_chunk(
        &self,
        stream_id: &str,
        sequence: u32,
    ) -> Result<Rc<MediaSession>, GatewayJsonError> {
        let entries = self.entries.borrow();
        let session = entries
            .get(stream_id)
            .ok_or(GatewayJsonError::UnknownStream)?;
        if sequence != session.next_sequence.get() {
            return Err(GatewayJsonError::OutOfOrder);
        }
        Ok(Rc::clone(session))
    }

    fn push_chunk(session: &MediaSession, request: JsonRef) -> Result<(), GatewayJsonError> {
        match session.commands.try_send(MediaCommand::Chunk(request)) {
            Ok(()) => {
                session
                    .next_sequence
                    .set(session.next_sequence.get().saturating_add(1));
                Ok(())
            }
            Err(TrySendError::Full(_command)) => Err(GatewayJsonError::Busy),
            Err(TrySendError::Closed(_command)) => Err(GatewayJsonError::UnknownStream),
        }
    }

    fn finish(&self, stream_id: &str, sequence: u32) -> Result<(), GatewayJsonError> {
        let mut entries = self.entries.borrow_mut();
        let session = entries
            .get(stream_id)
            .ok_or(GatewayJsonError::UnknownStream)?;
        if sequence != session.next_sequence.get() {
            return Err(GatewayJsonError::OutOfOrder);
        }
        match session.commands.try_send(MediaCommand::Finish(sequence)) {
            Ok(()) => {
                entries.remove(stream_id);
                Ok(())
            }
            Err(TrySendError::Full(_command)) => Err(GatewayJsonError::Busy),
            Err(TrySendError::Closed(_command)) => {
                entries.remove(stream_id);
                Err(GatewayJsonError::UnknownStream)
            }
        }
    }
}

/// Builds the JSON command handler for [`GatewaySendMedia`].
pub(crate) fn gateway_send_media_handler(
    sessions: Rc<MediaSessions>,
    jobs: Sender<MediaJob>,
) -> impl JsonHandler {
    move |_context, document: JsonRef, response: JsonWriter| {
        let sessions = Rc::clone(&sessions);
        let jobs = jobs.clone();
        async move {
            let request = document.deserialize::<MediaRequest<'_>>()?;
            match request {
                MediaRequest::Start {
                    stream_id,
                    sequence,
                    channel,
                    conversation_id,
                    thread_id,
                    kind,
                    filename,
                    mime_type,
                    caption,
                    reply_to,
                } => {
                    if sequence != 0
                        || !valid_stream_id(stream_id)
                        || !valid_required(channel)
                        || !valid_required(conversation_id)
                    {
                        return response
                            .write(&ErrorResponse(GatewayJsonError::InvalidRequest))
                            .await;
                    }
                    if sessions.entries.borrow().contains_key(stream_id) {
                        return response
                            .write(&ErrorResponse(GatewayJsonError::DuplicateStream))
                            .await;
                    }
                    if sessions.entries.borrow().len() >= STREAM_WORKERS {
                        return response.write(&ErrorResponse(GatewayJsonError::Busy)).await;
                    }

                    let (commands, receiver) = async_channel::bounded(CHUNK_QUEUE_CAPACITY);
                    let terminal_sequence = Rc::new(RefCell::new(None));
                    let mut target = MessageTarget::new(channel, conversation_id);
                    target.thread_id = thread_id.map(String::from);
                    let request = SendMediaRequest {
                        target,
                        body: BinaryBody::Stream(binary_stream(
                            receiver,
                            Rc::clone(&terminal_sequence),
                        )),
                        filename: filename.map(String::from),
                        mime_type: mime_type.map(String::from),
                        caption: caption.map(String::from),
                        reply_to: reply_to.map(String::from),
                    };
                    let job = MediaJob {
                        stream_id: String::from(stream_id),
                        terminal_sequence,
                        kind: kind.into(),
                        request,
                    };
                    match jobs.try_send(job) {
                        Ok(()) => {
                            sessions.entries.borrow_mut().insert(
                                String::from(stream_id),
                                Rc::new(MediaSession {
                                    next_sequence: Cell::new(1),
                                    commands,
                                }),
                            );
                            response
                                .write(&AckResponse {
                                    accepted_sequence: sequence,
                                })
                                .await
                        }
                        Err(TrySendError::Full(_job)) => {
                            response.write(&ErrorResponse(GatewayJsonError::Busy)).await
                        }
                        Err(TrySendError::Closed(_job)) => Err(RpcError::RegistryDropped),
                    }
                }
                MediaRequest::Chunk {
                    stream_id,
                    sequence,
                    content_base64,
                } => {
                    if !valid_stream_id(stream_id) {
                        return response
                            .write(&ErrorResponse(GatewayJsonError::InvalidRequest))
                            .await;
                    }
                    if visit_base64(content_base64, |_| Ok(())).is_err() {
                        return response
                            .write(&ErrorResponse(GatewayJsonError::InvalidRequest))
                            .await;
                    }
                    let session = match sessions.prepare_chunk(stream_id, sequence) {
                        Ok(session) => session,
                        Err(error) => return response.write(&ErrorResponse(error)).await,
                    };
                    let _ = content_base64;
                    let result = MediaSessions::push_chunk(&session, document);
                    match result {
                        Ok(()) => {
                            response
                                .write(&AckResponse {
                                    accepted_sequence: sequence,
                                })
                                .await
                        }
                        Err(error) => response.write(&ErrorResponse(error)).await,
                    }
                }
                MediaRequest::Finish {
                    stream_id,
                    sequence,
                } => {
                    if !valid_stream_id(stream_id) {
                        return response
                            .write(&ErrorResponse(GatewayJsonError::InvalidRequest))
                            .await;
                    }
                    let result = sessions.finish(stream_id, sequence);
                    match result {
                        Ok(()) => {
                            response
                                .write(&AckResponse {
                                    accepted_sequence: sequence,
                                })
                                .await
                        }
                        Err(error) => response.write(&ErrorResponse(error)).await,
                    }
                }
            }
        }
    }
}

pub(crate) async fn deliver_media_stream<const M: usize>(
    gateway: &MessageGateway,
    sessions: &MediaSessions,
    emitter: &EventEmitter<M>,
    job: MediaJob,
) -> Result<(), barracuda_event_router::EmitError> {
    let MediaJob {
        stream_id,
        terminal_sequence,
        kind,
        request,
    } = job;
    let result = match kind {
        MediaKind::File => gateway.send_file(request).await,
        MediaKind::Image => gateway.send_image(request).await,
        MediaKind::Audio => gateway.send_audio(request).await,
        MediaKind::Video => gateway.send_video(request).await,
    };
    sessions.remove(&stream_id);
    let completed_sequence = *terminal_sequence.borrow();
    let sequence = completed_sequence.unwrap_or_default();
    let mut terminal = match (&result, completed_sequence) {
        (Ok(receipt), Some(_)) => {
            TerminalEvent::completed(&stream_id, sequence, &receipt.message_id)
        }
        (Ok(_receipt), None) => {
            TerminalEvent::failed(&stream_id, sequence, GatewayJsonError::Delivery)
        }
        (Err(error), _) => TerminalEvent::failed(&stream_id, sequence, map_gateway_error(error)),
    };
    let event_input_bytes = event_input_capacity::<M>(GatewaySendMediaFinished::ID)?;
    if terminal
        .encoded_len()
        .map_or(true, |length| length > event_input_bytes)
    {
        terminal = TerminalEvent::failed(&stream_id, sequence, GatewayJsonError::InvalidReceipt);
    }
    emitter.emit::<GatewaySendMediaFinished>(&terminal).await
}

fn binary_stream(
    commands: Receiver<MediaCommand>,
    terminal_sequence: Rc<RefCell<Option<u32>>>,
) -> gateway::BinaryStream {
    Box::pin(stream::unfold(
        (commands, terminal_sequence),
        |(commands, terminal_sequence)| async move {
            match commands.recv().await {
                Ok(MediaCommand::Chunk(request)) => {
                    let bytes = match request.deserialize::<MediaRequest<'_>>() {
                        Ok(MediaRequest::Chunk { content_base64, .. }) => {
                            decode_base64(content_base64)
                                .map_err(|_| StreamError::failed("invalid Gateway media chunk"))
                        }
                        Ok(_) | Err(_) => Err(StreamError::failed("invalid Gateway media chunk")),
                    };
                    Some((bytes, (commands, terminal_sequence)))
                }
                Ok(MediaCommand::Finish(sequence)) => {
                    terminal_sequence.replace(Some(sequence));
                    None
                }
                Err(_closed) => None,
            }
        },
    ))
}

fn decode_base64(encoded: &str) -> Result<BinaryChunk, GatewayJsonError> {
    let mut output = BinaryChunk::empty_inline();
    visit_base64(encoded, |byte| {
        output
            .push(byte)
            .then_some(())
            .ok_or(GatewayJsonError::InvalidRequest)
    })?;
    Ok(output)
}

fn visit_base64(
    encoded: &str,
    mut push: impl FnMut(u8) -> Result<(), GatewayJsonError>,
) -> Result<(), GatewayJsonError> {
    if !encoded.len().is_multiple_of(4) {
        return Err(GatewayJsonError::InvalidRequest);
    }
    let group_count = encoded.len() / 4;
    for (index, chunk) in encoded.as_bytes().chunks_exact(4).enumerate() {
        let a = decode_base64_byte(*chunk.first().ok_or(GatewayJsonError::InvalidRequest)?)?;
        let b = decode_base64_byte(*chunk.get(1).ok_or(GatewayJsonError::InvalidRequest)?)?;
        let c = *chunk.get(2).ok_or(GatewayJsonError::InvalidRequest)?;
        let d = *chunk.get(3).ok_or(GatewayJsonError::InvalidRequest)?;
        let is_last = index.saturating_add(1) == group_count;
        if c == b'=' && (!is_last || d != b'=') {
            return Err(GatewayJsonError::InvalidRequest);
        }
        if d == b'=' && !is_last {
            return Err(GatewayJsonError::InvalidRequest);
        }
        let c_value = if c == b'=' { 0 } else { decode_base64_byte(c)? };
        let d_value = if d == b'=' { 0 } else { decode_base64_byte(d)? };
        if (c == b'=' && b & 0x0f != 0) || (d == b'=' && c_value & 0x03 != 0) {
            return Err(GatewayJsonError::InvalidRequest);
        }
        let word = (u32::from(a) << 18)
            | (u32::from(b) << 12)
            | (u32::from(c_value) << 6)
            | u32::from(d_value);
        push(((word >> 16) & 0xff) as u8)?;
        if c != b'=' {
            push(((word >> 8) & 0xff) as u8)?;
        }
        if d != b'=' {
            push((word & 0xff) as u8)?;
        }
    }
    Ok(())
}

fn decode_base64_byte(byte: u8) -> Result<u8, GatewayJsonError> {
    match byte {
        b'A'..=b'Z' => byte
            .checked_sub(b'A')
            .ok_or(GatewayJsonError::InvalidRequest),
        b'a'..=b'z' => byte
            .checked_sub(b'a')
            .and_then(|value| value.checked_add(26))
            .ok_or(GatewayJsonError::InvalidRequest),
        b'0'..=b'9' => byte
            .checked_sub(b'0')
            .and_then(|value| value.checked_add(52))
            .ok_or(GatewayJsonError::InvalidRequest),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(GatewayJsonError::InvalidRequest),
    }
}

enum TerminalOutcome<'a> {
    Completed(&'a str),
    Failed(GatewayJsonError),
}

struct TerminalEvent<'a> {
    stream_id: &'a str,
    sequence: u32,
    outcome: TerminalOutcome<'a>,
}

impl<'a> TerminalEvent<'a> {
    const fn completed(stream_id: &'a str, sequence: u32, message_id: &'a str) -> Self {
        Self {
            stream_id,
            sequence,
            outcome: TerminalOutcome::Completed(message_id),
        }
    }

    const fn failed(stream_id: &'a str, sequence: u32, error: GatewayJsonError) -> Self {
        Self {
            stream_id,
            sequence,
            outcome: TerminalOutcome::Failed(error),
        }
    }
}

impl EncodedJson for TerminalEvent<'_> {
    fn encode(&self, writer: &mut impl fmt::Write) -> fmt::Result {
        writer.write_str("{\"stream_id\":")?;
        write_json_string(writer, self.stream_id)?;
        write!(writer, ",\"sequence\":{},\"outcome\":", self.sequence)?;
        match self.outcome {
            TerminalOutcome::Completed(message_id) => {
                writer.write_str("\"completed\",\"message_id\":")?;
                write_json_string(writer, message_id)?;
            }
            TerminalOutcome::Failed(error) => {
                writer.write_str("\"failed\",\"error\":")?;
                write_json_string(writer, error.code())?;
            }
        }
        writer.write_char('}')
    }
}

impl JsonPayload for TerminalEvent<'_> {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        encoded_json_len(self)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_encoded_json(self, destination)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::decode_base64;
    use crate::json::GatewayJsonError;

    #[test]
    fn media_chunks_require_canonical_base64_and_use_the_request_lane_bound() {
        assert_eq!(
            decode_base64("AAH/gA==").map(|bytes| bytes.as_slice().to_vec()),
            Ok(vec![0, 1, 255, 128])
        );
        assert_eq!(decode_base64("AB=="), Err(GatewayJsonError::InvalidRequest));
        assert_eq!(
            decode_base64("not-base64"),
            Err(GatewayJsonError::InvalidRequest)
        );
        assert_eq!(
            decode_base64(&"A".repeat(324)).map(|bytes| bytes.len()),
            Ok(243)
        );
    }
}
