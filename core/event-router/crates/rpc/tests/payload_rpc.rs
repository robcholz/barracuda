#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use core::cell::Cell;
use std::rc::Rc;

use barracuda_rpc::{
    RpcAddress, RpcContext, RpcError, RpcFrame, RpcLaneStorage, RpcMethod, RpcPayloadReader,
    RpcPayloadWriter, RpcRegistry, RpcResult, RpcStream, Streaming, Unary,
};
use futures_lite::future::{block_on, poll_once};
use futures_util::{future, stream};

fn registry<const N: usize, const M: usize, const Q: usize>() -> RpcRegistry<N, M, Q> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<N, M, Q>::new()));
    RpcRegistry::new(lanes)
}

async fn send_payloads(mut writer: RpcPayloadWriter, payloads: Vec<Vec<u8>>) -> RpcResult<()> {
    for payload in payloads {
        writer.write_all(&payload).await?;
    }
    writer.close().await
}

async fn collect_successes(mut reader: RpcPayloadReader) -> RpcResult<Vec<Vec<u8>>> {
    let mut output = Vec::new();
    while let Some(value) = reader.read().await? {
        output.push(value.expect("method success").as_ref().to_vec());
    }
    Ok(output)
}

async fn invoke(
    writer: RpcPayloadWriter,
    reader: RpcPayloadReader,
    payloads: Vec<Vec<u8>>,
) -> RpcResult<Vec<Vec<u8>>> {
    let (write_result, read_result) =
        future::join(send_payloads(writer, payloads), collect_successes(reader)).await;
    write_result?;
    read_result
}

struct UnaryBytes;

impl RpcMethod for UnaryBytes {
    const ADDRESS: &'static str = "wire.unary";
    type Request = [u8; 7];
    type Response = [u8; 8];
    type Error = [u8; 6];
    type Input = Unary;
    type Output = Unary;
}

struct ChunkEcho;

impl RpcMethod for ChunkEcho {
    const ADDRESS: &'static str = "wire.chunks";
    type Request = [u8; 8];
    type Response = [u8; 8];
    type Error = [u8; 8];
    type Input = Streaming;
    type Output = Streaming;
}

async fn echo_chunks(
    _context: RpcContext,
    requests: RpcStream<RpcFrame<[u8; 8]>>,
) -> RpcResult<RpcStream<Result<[u8; 8], [u8; 8]>>> {
    let responses = stream::unfold(requests, |mut requests| async move {
        match requests.next().await {
            Some(Ok(request)) => {
                let response = request.view().copied().map(Ok);
                Some((response, requests))
            }
            Some(Err(error)) => Some((Err(error), requests)),
            None => None,
        }
    });
    Ok(RpcStream::new(responses))
}

#[test]
fn reserved_payload_is_written_and_read_in_place() {
    let registry = registry::<1, 64, 1>();
    let request_pointer = Rc::new(Cell::new(0_usize));
    let handler_pointer = Rc::clone(&request_pointer);
    registry
        .register::<UnaryBytes, _>(move |_context, request: RpcFrame<[u8; 7]>| {
            let pointer = Rc::clone(&handler_pointer);
            async move {
                assert_eq!(request.view()?.as_ptr() as usize, pointer.get());
                assert_eq!(request.view()?, b"request");
                Ok(Ok(*b"response"))
            }
        })
        .expect("register typed endpoint");

    let address = RpcAddress::try_from(UnaryBytes::ADDRESS).expect("valid address");
    let (mut writer, mut reader) = registry
        .client()
        .call_payload(&address)
        .expect("start payload call");

    block_on(async {
        let mut frame = writer.reserve().await.expect("reserve request frame");
        request_pointer.set(frame.as_mut().as_ptr() as usize);
        frame.as_mut().copy_from_slice(b"request");
        frame.commit(7).expect("commit request frame");
        writer.close().await.expect("close request input");

        let response = reader
            .read()
            .await
            .expect("read response")
            .expect("response frame")
            .expect("method success");
        assert_eq!(response.as_ref(), b"response");
    });
}

#[test]
fn write_all_chunks_unknown_sized_input_across_request_frames() {
    let registry = registry::<1, 64, 1>();
    registry
        .register::<ChunkEcho, _>(echo_chunks)
        .expect("register typed chunk endpoint");
    let address = RpcAddress::try_from(ChunkEcho::ADDRESS).expect("valid address");
    let (writer, reader) = registry
        .client()
        .call_payload(&address)
        .expect("start payload stream");

    assert_eq!(
        block_on(invoke(
            writer,
            reader,
            vec![b"chunk-01chunk-02chunk-03".to_vec()],
        )),
        Ok(vec![
            b"chunk-01".to_vec(),
            b"chunk-02".to_vec(),
            b"chunk-03".to_vec(),
        ])
    );
}

struct StreamFailure;

impl RpcMethod for StreamFailure {
    const ADDRESS: &'static str = "wire.failure";
    type Request = [u8; 8];
    type Response = [u8; 8];
    type Error = [u8; 8];
    type Input = Unary;
    type Output = Streaming;
}

#[test]
fn method_error_is_a_terminal_payload_frame() {
    let registry = registry::<1, 64, 1>();
    registry
        .register::<StreamFailure, _>(|_context, _request: RpcFrame<[u8; 8]>| async move {
            Ok(RpcStream::new(stream::iter([
                Ok(Ok(*b"before--")),
                Ok(Err(*b"failed--")),
                Ok(Ok(*b"after---")),
            ])))
        })
        .expect("register typed endpoint");
    let address = RpcAddress::try_from(StreamFailure::ADDRESS).expect("valid address");

    block_on(async {
        let (mut writer, mut reader) = registry
            .client()
            .call_payload(&address)
            .expect("start payload call");
        writer.write_all(b"request-").await.expect("write request");
        writer.close().await.expect("close request input");

        let success = reader
            .read()
            .await
            .expect("read success")
            .expect("success frame")
            .expect("method success");
        assert_eq!(success.as_ref(), b"before--");
        drop(success);

        let error = reader
            .read()
            .await
            .expect("read method error")
            .expect("method error frame")
            .expect_err("method error");
        assert_eq!(error.as_ref(), b"failed--");
        drop(error);
        assert!(reader.read().await.expect("terminal EOF").is_none());
    });
}

#[test]
fn payload_frame_retains_the_lane_until_drop() {
    block_on(async {
        let registry = registry::<1, 64, 2>();
        registry
            .register::<UnaryBytes, _>(|_context, _request: RpcFrame<[u8; 7]>| async move {
                Ok(Ok(*b"response"))
            })
            .expect("register typed endpoint");
        let address = RpcAddress::try_from(UnaryBytes::ADDRESS).expect("valid address");

        let (mut writer, mut reader) = registry
            .client()
            .call_payload(&address)
            .expect("start payload call");
        writer.write_all(b"request").await.expect("write request");
        writer.close().await.expect("close request input");
        let response = reader
            .read()
            .await
            .expect("read response")
            .expect("response frame")
            .expect("method success");

        let typed_call = registry
            .client()
            .call::<UnaryBytes>(*b"request")
            .expect("start typed call");
        assert!(poll_once(typed_call).await.is_none());

        drop(response);
        drop(reader);
        let typed_response = registry
            .client()
            .call::<UnaryBytes>(*b"request")
            .expect("restart typed call")
            .await
            .expect("typed transport success")
            .expect("typed method success");
        assert_eq!(typed_response.view(), Ok(b"response"));
    });
}

#[test]
fn prepared_payload_call_survives_endpoint_unregister() {
    let registry = registry::<1, 64, 1>();
    let registration = registry
        .register::<UnaryBytes, _>(|_context, _request: RpcFrame<[u8; 7]>| async move {
            Ok(Ok(*b"response"))
        })
        .expect("register typed endpoint");
    let address = RpcAddress::try_from(UnaryBytes::ADDRESS).expect("valid address");
    let client = registry.client();
    let (writer, reader) = client.call_payload(&address).expect("prepare payload call");

    registry
        .unregister(&registration)
        .expect("unregister exact endpoint");
    assert_eq!(
        block_on(invoke(writer, reader, vec![b"request".to_vec()])),
        Ok(vec![b"response".to_vec()])
    );

    assert!(matches!(
        client.call_payload(&address),
        Err(RpcError::NotFound(missing)) if missing == address
    ));
}

struct RawSelfCall;

impl RpcMethod for RawSelfCall {
    const ADDRESS: &'static str = "wire.self_call";
    type Request = [u8; 8];
    type Response = [u8; 8];
    type Error = [u8; 8];
    type Input = Unary;
    type Output = Unary;
}

#[test]
fn payload_call_obeys_typed_endpoint_self_call_protection() {
    let registry = registry::<2, 64, 2>();
    let address = RpcAddress::try_from(RawSelfCall::ADDRESS).expect("valid address");
    let handler_address = address.clone();
    registry
        .register::<RawSelfCall, _>(move |context: RpcContext, _request: RpcFrame<[u8; 8]>| {
            let address = handler_address.clone();
            async move {
                match context.client().call_payload(&address) {
                    Err(RpcError::DirectSelfCall(_)) => Ok(Ok(*b"rejected")),
                    Err(error) => Err(error),
                    Ok(_) => Ok(Ok(*b"bad-call")),
                }
            }
        })
        .expect("register typed endpoint");

    let (writer, reader) = registry
        .client()
        .call_payload(&address)
        .expect("start root call");
    assert_eq!(
        block_on(invoke(writer, reader, vec![b"root----".to_vec()])),
        Ok(vec![b"rejected".to_vec()])
    );
}

struct RawInner;

impl RpcMethod for RawInner {
    const ADDRESS: &'static str = "wire.inner";
    type Request = [u8; 8];
    type Response = [u8; 8];
    type Error = [u8; 8];
    type Input = Unary;
    type Output = Unary;
}

struct RawOuter;

impl RpcMethod for RawOuter {
    const ADDRESS: &'static str = "wire.outer";
    type Request = [u8; 8];
    type Response = [u8; 8];
    type Error = [u8; 8];
    type Input = Unary;
    type Output = Unary;
}

#[test]
fn nested_payload_write_uses_lane_deadlock_protection() {
    let registry = registry::<1, 64, 1>();
    registry
        .register::<RawInner, _>(|_context, request: RpcFrame<[u8; 8]>| async move {
            Ok(Ok(*request.view()?))
        })
        .expect("register inner endpoint");
    let inner_address = RpcAddress::try_from(RawInner::ADDRESS).expect("valid address");
    registry
        .register::<RawOuter, _>(move |context: RpcContext, request: RpcFrame<[u8; 8]>| {
            let inner_address = inner_address.clone();
            async move {
                drop(request);
                let (mut nested_writer, _nested_reader) =
                    context.client().call_payload(&inner_address)?;
                match nested_writer.write(b"nested--").await {
                    Err(RpcError::NestedLaneExhausted { limit: 1 }) => Ok(Ok(*b"blocked-")),
                    Err(error) => Err(error),
                    Ok(_) => Ok(Ok(*b"bad-call")),
                }
            }
        })
        .expect("register outer endpoint");
    let outer_address = RpcAddress::try_from(RawOuter::ADDRESS).expect("valid address");

    let (writer, reader) = registry
        .client()
        .call_payload(&outer_address)
        .expect("start outer call");
    assert_eq!(
        block_on(invoke(writer, reader, vec![b"root----".to_vec()])),
        Ok(vec![b"blocked-".to_vec()])
    );
}

struct FourBytes;

impl RpcMethod for FourBytes {
    const ADDRESS: &'static str = "wire.four_bytes";
    type Request = [u8; 4];
    type Response = [u8; 4];
    type Error = [u8; 4];
    type Input = Unary;
    type Output = Unary;
}

#[test]
fn oversized_write_returns_the_written_prefix() {
    let registry = registry::<1, 16, 1>();
    registry
        .register::<FourBytes, _>(|_context, request: RpcFrame<[u8; 4]>| async move {
            Ok(Ok(*request.view()?))
        })
        .expect("register typed endpoint");
    let address = RpcAddress::try_from(FourBytes::ADDRESS).expect("valid address");

    block_on(async {
        let (mut writer, mut reader) = registry
            .client()
            .call_payload(&address)
            .expect("start payload call");
        assert_eq!(writer.write(b"too-large").await, Ok(4));
        writer.close().await.expect("close request input");
        let response = reader
            .read()
            .await
            .expect("read response")
            .expect("response frame")
            .expect("method success");
        assert_eq!(response.as_ref(), b"too-");
    });
}

#[test]
fn reserve_rejects_commit_larger_than_the_method_frame() {
    let registry = registry::<1, 16, 1>();
    registry
        .register::<FourBytes, _>(|_context, request: RpcFrame<[u8; 4]>| async move {
            Ok(Ok(*request.view()?))
        })
        .expect("register typed endpoint");
    let address = RpcAddress::try_from(FourBytes::ADDRESS).expect("valid address");
    let (mut writer, reader) = registry
        .client()
        .call_payload(&address)
        .expect("start payload call");

    let frame = block_on(writer.reserve()).expect("reserve request frame");
    assert!(matches!(
        frame.commit(5),
        Err(RpcError::FrameTooLarge {
            size: 5,
            capacity: 4
        })
    ));
    drop(writer);
    drop(reader);
}

#[test]
fn dropped_reservation_publishes_nothing_and_can_be_reused() {
    let registry = registry::<1, 16, 1>();
    registry
        .register::<FourBytes, _>(|_context, request: RpcFrame<[u8; 4]>| async move {
            Ok(Ok(*request.view()?))
        })
        .expect("register typed endpoint");
    let address = RpcAddress::try_from(FourBytes::ADDRESS).expect("valid address");

    block_on(async {
        let (mut writer, mut reader) = registry
            .client()
            .call_payload(&address)
            .expect("start payload call");
        drop(writer.reserve().await.expect("first reservation"));
        assert_eq!(writer.write(b"done").await, Ok(4));
        writer.close().await.expect("close request input");
        let response = reader
            .read()
            .await
            .expect("read response")
            .expect("response frame")
            .expect("method success");
        assert_eq!(response.as_ref(), b"done");
    });
}

#[test]
fn typed_handler_rejects_an_incomplete_payload_frame() {
    let registry = registry::<1, 16, 1>();
    let handler_calls = Rc::new(Cell::new(0_u32));
    let observed_handler_calls = Rc::clone(&handler_calls);
    registry
        .register::<FourBytes, _>(move |_context, request: RpcFrame<[u8; 4]>| {
            let calls = Rc::clone(&observed_handler_calls);
            async move {
                calls.set(calls.get().saturating_add(1));
                Ok(Ok(*request.view()?))
            }
        })
        .expect("register typed endpoint");
    let address = RpcAddress::try_from(FourBytes::ADDRESS).expect("valid address");

    block_on(async {
        let (mut writer, mut reader) = registry
            .client()
            .call_payload(&address)
            .expect("start payload call");
        assert_eq!(writer.write(&[1, 2, 3]).await, Ok(3));
        assert!(matches!(
            writer.close().await,
            Err(RpcError::InvalidMessageFrame { .. })
        ));
        assert!(matches!(
            reader.read().await,
            Err(RpcError::InvalidMessageFrame { .. })
        ));
    });
    assert_eq!(handler_calls.get(), 0);
}

struct EmptyRequest;

impl RpcMethod for EmptyRequest {
    const ADDRESS: &'static str = "wire.empty";
    type Request = ();
    type Response = [u8; 1];
    type Error = [u8; 1];
    type Input = Unary;
    type Output = Unary;
}

#[test]
fn zero_sized_request_uses_explicit_reserve_and_commit() {
    let registry = registry::<1, 16, 1>();
    registry
        .register::<EmptyRequest, _>(|_context, request: RpcFrame<()>| async move {
            request.view()?;
            Ok(Ok([7]))
        })
        .expect("register empty request endpoint");
    let address = RpcAddress::try_from(EmptyRequest::ADDRESS).expect("valid address");

    block_on(async {
        let (mut writer, mut reader) = registry
            .client()
            .call_payload(&address)
            .expect("start payload call");
        assert_eq!(writer.write(&[1]).await, Ok(0));
        assert_eq!(
            writer.write_all(&[1]).await,
            Err(RpcError::PayloadWriteZero)
        );
        let mut frame = writer.reserve().await.expect("reserve empty frame");
        assert!(frame.as_mut().is_empty());
        frame.commit(0).expect("commit empty frame");
        writer.close().await.expect("close request input");

        let response = reader
            .read()
            .await
            .expect("read response")
            .expect("response frame")
            .expect("method success");
        assert_eq!(response.as_ref(), &[7]);
    });
}
