#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use core::cell::Cell;
use core::task::Poll;
use std::rc::Rc;

use barracuda_rpc::{
    RpcAddress, RpcContext, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, RpcResult, RpcStream,
    Streaming, Unary,
};
use futures_lite::future::{block_on, poll_once};
use futures_util::stream;

struct GatedEcho;

impl RpcMethod for GatedEcho {
    const ADDRESS: &'static str = "backpressure.gated";
    type Request = [u8; 8];
    type Response = [u8; 8];
    type Error = ();
    type Input = Streaming;
    type Output = Streaming;
}

async fn echo_requests(
    _context: RpcContext,
    requests: RpcStream<RpcFrame<[u8; 8]>>,
    gate: Rc<Cell<bool>>,
) -> RpcResult<RpcStream<Result<[u8; 8], ()>>> {
    core::future::poll_fn(|_context| {
        if gate.get() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
    Ok(RpcStream::new(stream::unfold(
        requests,
        |mut requests| async move {
            match requests.next().await {
                Some(Ok(request)) => Some((request.view().copied().map(Ok), requests)),
                Some(Err(error)) => Some((Err(error), requests)),
                None => None,
            }
        },
    )))
}

#[test]
fn request_pipe_holds_exactly_one_unconsumed_frame() {
    block_on(async {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 8, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        let gate = Rc::new(Cell::new(false));
        let handler_gate = Rc::clone(&gate);
        registry
            .register::<GatedEcho, _>(
                move |context: RpcContext, requests: RpcStream<RpcFrame<[u8; 8]>>| {
                    echo_requests(context, requests, Rc::clone(&handler_gate))
                },
            )
            .expect("register gated endpoint");
        let address = RpcAddress::try_from(GatedEcho::ADDRESS).expect("valid address");
        let (mut writer, mut reader) = registry
            .client()
            .call_payload(&address)
            .expect("prepare payload call");

        assert_eq!(writer.write(b"frame-01").await, Ok(8));
        let mut second_write = Box::pin(writer.write(b"frame-02"));
        assert!(poll_once(second_write.as_mut()).await.is_none());

        gate.set(true);
        assert_eq!(second_write.await, Ok(8));
        writer.close().await.expect("close request stream");

        let first = reader
            .read()
            .await
            .expect("read first response")
            .expect("first response frame")
            .expect("first method success");
        assert_eq!(first.as_ref(), b"frame-01");
        drop(first);
        let second = reader
            .read()
            .await
            .expect("read second response")
            .expect("second response frame")
            .expect("second method success");
        assert_eq!(second.as_ref(), b"frame-02");
        drop(second);
        assert!(reader.read().await.expect("read response EOF").is_none());
    });
}

struct Burst;

impl RpcMethod for Burst {
    const ADDRESS: &'static str = "backpressure.burst";
    type Request = [u8; 1];
    type Response = [u8; 8];
    type Error = ();
    type Input = Unary;
    type Output = Streaming;
}

#[test]
fn borrowed_response_frame_blocks_the_next_response_frame() {
    block_on(async {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 8, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        registry
            .register::<Burst, _>(|_context, _request: RpcFrame<[u8; 1]>| async move {
                Ok(RpcStream::new(stream::iter([
                    Ok(Ok(*b"frame-01")),
                    Ok(Ok(*b"frame-02")),
                ])))
            })
            .expect("register burst endpoint");
        let address = RpcAddress::try_from(Burst::ADDRESS).expect("valid address");
        let (mut writer, mut reader) = registry
            .client()
            .call_payload(&address)
            .expect("prepare payload call");
        writer.write_all(&[1]).await.expect("write request");
        writer.close().await.expect("close request stream");

        let first = reader
            .read()
            .await
            .expect("read first response")
            .expect("first response frame")
            .expect("first method success");
        assert_eq!(first.as_ref(), b"frame-01");

        let mut second_read = Box::pin(reader.read());
        assert!(poll_once(second_read.as_mut()).await.is_none());
        drop(first);

        let second = second_read
            .await
            .expect("read second response after releasing first")
            .expect("second response frame")
            .expect("second method success");
        assert_eq!(second.as_ref(), b"frame-02");
        drop(second);
        assert!(reader.read().await.expect("read response EOF").is_none());
    });
}
