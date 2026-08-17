//! Category: typed RPC calls and request/response cardinality.

use barracuda_rpc::{
    RpcContext, RpcError, RpcFrame, RpcHandler, RpcHandlerFuture, RpcHandlerInput,
    RpcHandlerOutput, RpcLaneStorage, RpcMethod, RpcRegistry, RpcResult, RpcStream, Streaming,
    Unary,
};
use futures_util::stream;
use static_cell::ConstStaticCell;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

static RPC_LANES: ConstStaticCell<RpcLaneStorage<4, 4_096, 8>> =
    ConstStaticCell::new(RpcLaneStorage::new());

#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
struct Number {
    value: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
struct Total {
    value: u32,
}

struct UnaryUnary;

impl RpcMethod for UnaryUnary {
    const ADDRESS: &'static str = "example.unary_unary";

    type Request = Number;
    type Response = Total;
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

struct AddOffset {
    offset: u32,
}

// Implementing the handler trait directly allows its future to borrow handler
// state. Stateless handlers can use the closure shorthand shown below.
impl RpcHandler<UnaryUnary> for AddOffset {
    fn call<'a>(
        &'a self,
        _context: RpcContext,
        request: RpcHandlerInput<UnaryUnary>,
    ) -> RpcHandlerFuture<'a, RpcHandlerOutput<UnaryUnary>> {
        Box::pin(async move {
            Ok(Ok(Total {
                value: request.view()?.value.saturating_add(self.offset),
            }))
        })
    }
}

struct UnaryStream;

impl RpcMethod for UnaryStream {
    const ADDRESS: &'static str = "example.unary_stream";

    type Request = Number;
    type Response = Total;
    type Error = ();
    type Input = Unary;
    type Output = Streaming;
}

struct StreamUnary;

impl RpcMethod for StreamUnary {
    const ADDRESS: &'static str = "example.stream_unary";

    type Request = Number;
    type Response = Total;
    type Error = ();
    type Input = Streaming;
    type Output = Unary;
}

struct StreamStream;

impl RpcMethod for StreamStream {
    const ADDRESS: &'static str = "example.stream_stream";

    type Request = Number;
    type Response = Total;
    type Error = ();
    type Input = Streaming;
    type Output = Streaming;
}

fn request_stream<I>(values: I) -> RpcStream<Number>
where
    I: IntoIterator<Item = u32>,
    I::IntoIter: 'static,
{
    RpcStream::new(stream::iter(
        values.into_iter().map(|value| Ok(Number { value })),
    ))
}

fn success<T, E>(outcome: Result<T, E>) -> RpcResult<T> {
    outcome.map_err(|_| RpcError::InvalidFrameState)
}

async fn collect(
    mut stream: RpcStream<Result<RpcFrame<Total>, RpcFrame<()>>>,
) -> RpcResult<Vec<Total>> {
    let mut values = Vec::new();
    while let Some(value) = stream.next().await {
        values.push(*success(value?)?.view()?);
    }
    Ok(values)
}

async fn run() -> RpcResult<()> {
    let lanes = RPC_LANES.take();
    let registry = RpcRegistry::new(lanes);

    registry.register::<UnaryUnary, _>(AddOffset { offset: 1 })?;

    registry.register::<UnaryStream, _>(|_context, request: RpcFrame<Number>| async move {
        let value = request.view()?.value;
        Ok(RpcStream::new(stream::iter([
            Ok(Ok(Total { value })),
            Ok(Ok(Total {
                value: value.saturating_add(1),
            })),
        ])))
    })?;

    registry.register::<StreamUnary, _>(
        |_context, mut requests: RpcStream<RpcFrame<Number>>| async move {
            let mut total = 0_u32;
            while let Some(request) = requests.next().await {
                total = total.saturating_add(request?.view()?.value);
            }
            Ok(Ok(Total { value: total }))
        },
    )?;

    registry.register::<StreamStream, _>(
        |_context, requests: RpcStream<RpcFrame<Number>>| async move {
            let responses = stream::unfold(requests, |mut requests| async move {
                match requests.next().await {
                    Some(Ok(request)) => match request.view() {
                        Ok(request) => Some((
                            Ok(Ok(Total {
                                value: request.value.saturating_mul(2),
                            })),
                            requests,
                        )),
                        Err(error) => Some((Err(error), requests)),
                    },
                    Some(Err(error)) => Some((Err(error), requests)),
                    None => None,
                }
            });
            Ok(RpcStream::new(responses))
        },
    )?;

    // Registry discovery returns stable, sorted snapshots. Each discovered
    // group can be used to list its currently registered RPC addresses.
    for group in registry.groups() {
        println!("RPC group: {group}");
        for address in registry.rpcs(&group) {
            println!("  {address}");
        }
    }

    let client = registry.client();

    // Unary -> Unary returns a self-driving future.
    let unary = success(client.call::<UnaryUnary>(Number { value: 41 })?.await?)?;
    assert_eq!(unary.view()?, &Total { value: 42 });

    // Unary -> Stream returns a self-driving RpcStream.
    let unary_stream = collect(client.call::<UnaryStream>(Number { value: 7 })?).await?;
    assert_eq!(unary_stream, vec![Total { value: 7 }, Total { value: 8 }]);

    // Stream -> Unary accepts RpcStream<Request>.
    let stream_unary = client
        .call::<StreamUnary>(request_stream([2, 3, 4]))?
        .await?;
    let stream_unary = success(stream_unary)?;
    assert_eq!(stream_unary.view()?, &Total { value: 9 });

    // Stream -> Stream can transform requests incrementally without buffering
    // the complete request or response body.
    let stream_stream = collect(client.call::<StreamStream>(request_stream([3, 5, 8]))?).await?;
    assert_eq!(
        stream_stream,
        vec![Total { value: 6 }, Total { value: 10 }, Total { value: 16 },]
    );

    println!("all four typed RPC shapes completed");
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> RpcResult<()> {
    run().await
}
