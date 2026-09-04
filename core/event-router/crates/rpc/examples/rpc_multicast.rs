//! Category: typed and runtime-addressed request-side multicast.

use alloc::rc::Rc;
use core::cell::Cell;

use barracuda_rpc::{
    RpcAddress, RpcContext, RpcFrame, RpcLaneStorage, RpcMethod, RpcMulticastBranch,
    RpcPayloadWriter, RpcRegistry, RpcResult, RpcStream, Streaming, Unary,
};
use futures_util::{join, stream};
use static_cell::ConstStaticCell;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

extern crate alloc;

static RPC_LANES: ConstStaticCell<RpcLaneStorage<2, 64, 4>> =
    ConstStaticCell::new(RpcLaneStorage::new());

// --- Typed unary multicast -------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, TryFromBytes)]
struct Number {
    value: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, TryFromBytes)]
struct Doubled {
    value: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, TryFromBytes)]
struct Squared {
    value: u32,
}

struct Double;

impl RpcMethod for Double {
    const ADDRESS: &'static str = "math.double";
    type Request = Number;
    type Response = Doubled;
    type Error = [u8; 1];
    type Input = Unary;
    type Output = Unary;
}

struct Square;

impl RpcMethod for Square {
    const ADDRESS: &'static str = "math.square";
    type Request = Number;
    type Response = Squared;
    type Error = [u8; 2];
    type Input = Unary;
    type Output = Unary;
}

async fn expect_response(branch: &mut RpcMulticastBranch, expected: &[u8]) -> RpcResult<()> {
    let response = branch
        .reader_mut()
        .read()
        .await?
        .ok_or(barracuda_rpc::RpcError::MissingUnaryFrame)?
        .map_err(|_method_error| barracuda_rpc::RpcError::InvalidFrameState)?;
    assert_eq!(response.as_ref(), expected);
    drop(response);
    assert!(branch.reader_mut().read().await?.is_none());
    Ok(())
}

async fn typed_multicast(registry: &RpcRegistry<2, 64, 4>) -> RpcResult<()> {
    let addresses = [
        RpcAddress::try_from(Double::ADDRESS)?,
        RpcAddress::try_from(Square::ADDRESS)?,
    ];
    let branches = registry
        .client()
        .multicast::<Number, Unary>(&addresses, Number { value: 6 })?;
    let mut branches = branches.into_iter();
    let mut doubled = branches
        .next()
        .ok_or(barracuda_rpc::RpcError::InvalidLaneState)?;
    let mut squared = branches
        .next()
        .ok_or(barracuda_rpc::RpcError::InvalidLaneState)?;

    // The input is typed once. Each target may still define a different
    // Response, Error, and Output contract, so responses remain wire branches.
    expect_response(&mut doubled, Doubled { value: 12 }.as_bytes()).await?;
    expect_response(&mut squared, Squared { value: 36 }.as_bytes()).await?;
    println!("typed multicast: one Number produced Doubled and Squared");
    Ok(())
}

// --- Runtime-addressed streaming multicast --------------------------------

struct EchoChunks;

impl RpcMethod for EchoChunks {
    const ADDRESS: &'static str = "chunks.echo";
    type Request = [u8; 8];
    type Response = [u8; 8];
    type Error = [u8; 1];
    type Input = Streaming;
    type Output = Streaming;
}

struct ChecksumChunks;

impl RpcMethod for ChecksumChunks {
    const ADDRESS: &'static str = "chunks.checksum";
    type Request = [u8; 8];
    type Response = [u8; 4];
    type Error = [u8; 2];
    type Input = Streaming;
    type Output = Streaming;
}

async fn send(mut writer: RpcPayloadWriter) -> RpcResult<()> {
    writer.write_all(b"chunk-01chunk-02").await?;
    writer.close().await
}

async fn receive_echo(mut branch: RpcMulticastBranch) -> RpcResult<usize> {
    let mut count = 0_usize;
    while let Some(response) = branch.reader_mut().read().await? {
        let frame = response.map_err(|_method_error| barracuda_rpc::RpcError::InvalidFrameState)?;
        println!("{}: {:?}", branch.address(), frame.as_ref());
        count = count.saturating_add(1);
    }
    Ok(count)
}

async fn receive_checksums(mut branch: RpcMulticastBranch) -> RpcResult<usize> {
    let mut count = 0_usize;
    while let Some(response) = branch.reader_mut().read().await? {
        let frame = response.map_err(|_method_error| barracuda_rpc::RpcError::InvalidFrameState)?;
        let bytes = <[u8; 4]>::try_from(frame.as_ref())
            .map_err(|_size_error| barracuda_rpc::RpcError::InvalidFrameState)?;
        println!(
            "{}: checksum {}",
            branch.address(),
            u32::from_le_bytes(bytes)
        );
        count = count.saturating_add(1);
    }
    Ok(count)
}

async fn payload_multicast(registry: &RpcRegistry<2, 64, 4>) -> RpcResult<()> {
    let echo_pointer = Rc::new(Cell::new(0_usize));
    let checksum_pointer = Rc::new(Cell::new(0_usize));

    let observed_pointer = Rc::clone(&echo_pointer);
    registry.register::<EchoChunks, _>(
        "system",
        move |_context: RpcContext, requests: RpcStream<RpcFrame<[u8; 8]>>| {
            let pointer = Rc::clone(&observed_pointer);
            async move {
                Ok(RpcStream::new(stream::unfold(
                    (requests, pointer),
                    |(mut requests, pointer)| async move {
                        requests.next().await.map(|request| {
                            let response = request.and_then(|frame| {
                                pointer.set(frame.view()?.as_ptr() as usize);
                                frame.view().copied().map(Ok)
                            });
                            (response, (requests, pointer))
                        })
                    },
                )))
            }
        },
    )?;

    let observed_pointer = Rc::clone(&checksum_pointer);
    registry.register::<ChecksumChunks, _>(
        "system",
        move |_context: RpcContext, requests: RpcStream<RpcFrame<[u8; 8]>>| {
            let pointer = Rc::clone(&observed_pointer);
            async move {
                Ok(RpcStream::new(stream::unfold(
                    (requests, pointer),
                    |(mut requests, pointer)| async move {
                        requests.next().await.map(|request| {
                            let response = request.and_then(|frame| {
                                let bytes = frame.view()?;
                                pointer.set(bytes.as_ptr() as usize);
                                let checksum = bytes
                                    .iter()
                                    .fold(0_u32, |sum, byte| sum.saturating_add(u32::from(*byte)));
                                Ok(Ok(checksum.to_le_bytes()))
                            });
                            (response, (requests, pointer))
                        })
                    },
                )))
            }
        },
    )?;

    let addresses = [
        RpcAddress::try_from(EchoChunks::ADDRESS)?,
        RpcAddress::try_from(ChecksumChunks::ADDRESS)?,
    ];
    let (writer, branches) = registry.client().multicast_payload(&addresses)?;
    let mut branches = branches.into_iter();
    let echo = branches
        .next()
        .ok_or(barracuda_rpc::RpcError::InvalidLaneState)?;
    let checksums = branches
        .next()
        .ok_or(barracuda_rpc::RpcError::InvalidLaneState)?;

    // Sending and both independent response readers are polled cooperatively.
    // The next request frame cannot reuse storage until both handlers release
    // the current shared frame.
    let (sent, echoed, checksummed) = join!(
        send(writer),
        receive_echo(echo),
        receive_checksums(checksums)
    );
    sent?;
    assert_eq!(echoed?, 2);
    assert_eq!(checksummed?, 2);
    assert_ne!(echo_pointer.get(), 0);
    assert_eq!(echo_pointer.get(), checksum_pointer.get());

    println!("both handlers borrowed the same request frame address");
    Ok(())
}

async fn run() -> RpcResult<()> {
    let registry = RpcRegistry::new(RPC_LANES.take());
    registry.register::<Double, _>("system", |_context, request: RpcFrame<Number>| async move {
        Ok(Ok(Doubled {
            value: request.view()?.value.saturating_mul(2),
        }))
    })?;
    registry.register::<Square, _>("system", |_context, request: RpcFrame<Number>| async move {
        let value = request.view()?.value;
        Ok(Ok(Squared {
            value: value.saturating_mul(value),
        }))
    })?;

    typed_multicast(&registry).await?;
    payload_multicast(&registry).await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> RpcResult<()> {
    run().await
}
