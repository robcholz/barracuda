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
    MessageGateway, MessageTarget, SendStream, SendStreamField, SendStreamFrame, SendStreamRequest,
    StreamBoundary, StreamError,
};
use serde::Deserialize;

use crate::component::STREAM_WORKERS;
use crate::gateway_send::map_gateway_error;
use crate::json::{
    encoded_json_len, event_input_capacity, valid_required, valid_stream_id, write_encoded_json,
    write_json_string, AckResponse, EncodedJson, ErrorResponse, GatewayJsonError, FRAME_CAPACITY,
};

const CHUNK_QUEUE_CAPACITY: usize = 2;

/// Applies one bounded command to an outbound text stream.
pub struct GatewaySendStream;

impl JsonRpcSchema for GatewaySendStream {
    const ADDRESS: &'static str = "gateway.send_stream";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("send_stream", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("send_stream", response);
    const MAX_REQUEST_BYTES: usize = FRAME_CAPACITY;
    const MAX_RESPONSE_BYTES: usize = 128;
}

/// Terminal outcome for one accepted outbound text stream.
pub struct GatewaySendStreamFinished;

impl Event for GatewaySendStreamFinished {
    const ID: &'static str = "gateway.send_stream.finished";
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum StreamRequest<'a> {
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
        #[serde(default, borrow)]
        reply_to: Option<&'a str>,
    },
    Chunk {
        #[serde(borrow)]
        stream_id: &'a str,
        sequence: u32,
        field: SendStreamField,
        boundary: StreamBoundary,
        #[serde(borrow)]
        text: &'a str,
    },
    Finish {
        #[serde(borrow)]
        stream_id: &'a str,
        sequence: u32,
    },
}

enum StreamCommand {
    Chunk(JsonRef),
    Finish(u32),
}

struct TextSession {
    next_sequence: Cell<u32>,
    commands: Sender<StreamCommand>,
}

pub(crate) struct TextJob {
    stream_id: String,
    terminal_sequence: Rc<RefCell<Option<u32>>>,
    target: MessageTarget,
    reply_to: Option<String>,
    commands: Receiver<StreamCommand>,
}

#[derive(Default)]
pub(crate) struct TextSessions {
    entries: RefCell<BTreeMap<String, Rc<TextSession>>>,
}

impl TextSessions {
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
    ) -> Result<Rc<TextSession>, GatewayJsonError> {
        let entries = self.entries.borrow();
        let session = entries
            .get(stream_id)
            .ok_or(GatewayJsonError::UnknownStream)?;
        if sequence != session.next_sequence.get() {
            return Err(GatewayJsonError::OutOfOrder);
        }
        Ok(Rc::clone(session))
    }

    fn push_chunk(session: &TextSession, request: JsonRef) -> Result<(), GatewayJsonError> {
        match session.commands.try_send(StreamCommand::Chunk(request)) {
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
        match session.commands.try_send(StreamCommand::Finish(sequence)) {
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

/// Builds the JSON command handler for [`GatewaySendStream`].
pub(crate) fn gateway_send_stream_handler(
    sessions: Rc<TextSessions>,
    jobs: Sender<TextJob>,
) -> impl JsonHandler {
    move |_context, document: JsonRef, response: JsonWriter| {
        let sessions = Rc::clone(&sessions);
        let jobs = jobs.clone();
        async move {
            let request = document.deserialize::<StreamRequest<'_>>()?;
            match request {
                StreamRequest::Start {
                    stream_id,
                    sequence,
                    channel,
                    conversation_id,
                    thread_id,
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
                    let job = TextJob {
                        stream_id: String::from(stream_id),
                        terminal_sequence: Rc::clone(&terminal_sequence),
                        target,
                        reply_to: reply_to.map(String::from),
                        commands: receiver,
                    };
                    match jobs.try_send(job) {
                        Ok(()) => {
                            sessions.entries.borrow_mut().insert(
                                String::from(stream_id),
                                Rc::new(TextSession {
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
                StreamRequest::Chunk {
                    stream_id,
                    sequence,
                    field,
                    boundary,
                    text,
                } => {
                    if !valid_stream_id(stream_id) {
                        return response
                            .write(&ErrorResponse(GatewayJsonError::InvalidRequest))
                            .await;
                    }
                    let session = match sessions.prepare_chunk(stream_id, sequence) {
                        Ok(session) => session,
                        Err(error) => return response.write(&ErrorResponse(error)).await,
                    };
                    let _ = (field, boundary, text);
                    let result = TextSessions::push_chunk(&session, document);
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
                StreamRequest::Finish {
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

pub(crate) async fn deliver_text_stream<const M: usize>(
    gateway: &MessageGateway,
    sessions: &TextSessions,
    emitter: &EventEmitter<M>,
    job: TextJob,
) -> Result<(), barracuda_event_router::EmitError> {
    let stream_id = job.stream_id;
    let terminal_sequence = Rc::clone(&job.terminal_sequence);
    let frames = command_stream(job.commands, Rc::clone(&terminal_sequence));
    let request = SendStreamRequest {
        target: job.target,
        frames,
        reply_to: job.reply_to,
    };
    let result = gateway.send_stream(request).await;
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
    let event_input_bytes = event_input_capacity::<M>(GatewaySendStreamFinished::ID)?;
    if terminal
        .encoded_len()
        .map_or(true, |length| length > event_input_bytes)
    {
        terminal = TerminalEvent::failed(&stream_id, sequence, GatewayJsonError::InvalidReceipt);
    }
    emitter.emit::<GatewaySendStreamFinished>(&terminal).await
}

fn command_stream(
    commands: Receiver<StreamCommand>,
    terminal_sequence: Rc<RefCell<Option<u32>>>,
) -> SendStream {
    Box::pin(stream::unfold(
        (commands, terminal_sequence),
        |(commands, terminal_sequence)| async move {
            match commands.recv().await {
                Ok(StreamCommand::Chunk(request)) => {
                    let frame = match request.deserialize::<StreamRequest<'_>>() {
                        Ok(StreamRequest::Chunk {
                            field,
                            boundary,
                            text,
                            ..
                        }) => SendStreamFrame::inline(field, boundary, text)
                            .ok_or_else(|| StreamError::failed("Gateway text chunk exceeds lane")),
                        Ok(_) | Err(_) => Err(StreamError::failed("invalid Gateway text chunk")),
                    };
                    Some((frame, (commands, terminal_sequence)))
                }
                Ok(StreamCommand::Finish(sequence)) => {
                    terminal_sequence.replace(Some(sequence));
                    None
                }
                Err(_closed) => None,
            }
        },
    ))
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
    #![allow(clippy::expect_used)]

    use alloc::string::String;

    use super::{StreamCommand, TextSession, TextSessions, CHUNK_QUEUE_CAPACITY};
    use crate::json::GatewayJsonError;

    #[test]
    fn text_session_enforces_sequence_and_bounded_backpressure() {
        let sessions = TextSessions::default();
        let (commands, receiver) = async_channel::bounded(CHUNK_QUEUE_CAPACITY);
        sessions.entries.borrow_mut().insert(
            String::from("stream-1"),
            alloc::rc::Rc::new(TextSession {
                next_sequence: core::cell::Cell::new(1),
                commands,
            }),
        );

        assert_eq!(
            sessions.prepare_chunk("stream-1", 2).map(|_| ()),
            Err(GatewayJsonError::OutOfOrder)
        );
        let session = sessions
            .prepare_chunk("stream-1", 1)
            .expect("first sequence is accepted");
        session
            .commands
            .try_send(StreamCommand::Finish(1))
            .expect("first queue slot");
        session.next_sequence.set(2);
        session
            .commands
            .try_send(StreamCommand::Finish(2))
            .expect("second queue slot");
        session.next_sequence.set(3);
        assert_eq!(sessions.finish("stream-1", 3), Err(GatewayJsonError::Busy));
        assert!(matches!(receiver.try_recv(), Ok(StreamCommand::Finish(1))));
        assert_eq!(sessions.finish("stream-1", 3), Ok(()));
    }
}
