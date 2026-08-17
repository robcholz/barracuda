#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_rpc::{
    RpcContext, RpcError, RpcFrame, RpcGroup, RpcLaneStorage, RpcMessage, RpcMethod, RpcRegistry,
    RpcResult, RpcStream, Streaming, Unary,
};
use futures_lite::future::{block_on, poll_once};
use futures_util::stream;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
struct Number {
    value: u32,
    label: [u8; 40],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
struct Total {
    value: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
struct MethodFailure {
    code: u32,
}

fn registry() -> RpcRegistry<4, 4_096, 8> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 4_096, 8>::new()));
    RpcRegistry::new(lanes)
}

struct UnaryUnaryMethod;

impl RpcMethod for UnaryUnaryMethod {
    const ADDRESS: &'static str = "typed.unary_unary";
    type Request = Number;
    type Response = Total;
    type Error = MethodFailure;
    type Input = Unary;
    type Output = Unary;
}

struct UnaryStreamMethod;

impl RpcMethod for UnaryStreamMethod {
    const ADDRESS: &'static str = "typed.unary_stream";
    type Request = Number;
    type Response = Total;
    type Error = MethodFailure;
    type Input = Unary;
    type Output = Streaming;
}

struct StreamUnaryMethod;

impl RpcMethod for StreamUnaryMethod {
    const ADDRESS: &'static str = "typed.stream_unary";
    type Request = Number;
    type Response = Total;
    type Error = MethodFailure;
    type Input = Streaming;
    type Output = Unary;
}

struct StreamStreamMethod;

impl RpcMethod for StreamStreamMethod {
    const ADDRESS: &'static str = "typed.stream_stream";
    type Request = Number;
    type Response = Total;
    type Error = MethodFailure;
    type Input = Streaming;
    type Output = Streaming;
}

struct OtherGroupMethod;

impl RpcMethod for OtherGroupMethod {
    const ADDRESS: &'static str = "other.status";
    type Request = Number;
    type Response = Total;
    type Error = MethodFailure;
    type Input = Unary;
    type Output = Unary;
}

fn number(value: u32) -> Number {
    Number {
        value,
        label: [b'x'; 40],
    }
}

fn input_stream<I>(values: I) -> RpcStream<Number>
where
    I: IntoIterator<Item = u32>,
    I::IntoIter: 'static,
{
    RpcStream::new(stream::iter(
        values.into_iter().map(|value| Ok(number(value))),
    ))
}

async fn collect(
    mut values: RpcStream<Result<RpcFrame<Total>, RpcFrame<MethodFailure>>>,
) -> RpcResult<Vec<Total>> {
    let mut output = Vec::new();
    while let Some(value) = values.next().await {
        output.push(*value?.expect("method success").view()?);
    }
    Ok(output)
}

fn copy_outcome<T, E>(outcome: Result<RpcFrame<T>, RpcFrame<E>>) -> RpcResult<Result<T, E>>
where
    T: RpcMessage + Copy,
    E: RpcMessage + Copy,
{
    match outcome {
        Ok(response) => Ok(Ok(*response.view()?)),
        Err(error) => Ok(Err(*error.view()?)),
    }
}

#[test]
fn client_does_not_retain_the_registry() {
    let client = {
        let registry = registry();
        registry.client()
    };

    assert!(matches!(
        client.call::<UnaryUnaryMethod>(number(1)),
        Err(RpcError::RegistryDropped)
    ));
}

#[test]
fn registry_discovers_sorted_groups_and_rpcs_as_snapshots() {
    let registry = registry();
    let unary = registry
        .register::<UnaryUnaryMethod, _>(|_context, request: RpcFrame<Number>| async move {
            Ok(Ok(Total {
                value: request.view()?.value,
            }))
        })
        .expect("register typed unary endpoint");
    let lease = registry
        .register::<LeaseMethod, _>(|_context, request: RpcFrame<Number>| async move {
            Ok(Ok(*request.view()?))
        })
        .expect("register typed lease endpoint");
    registry
        .register::<OtherGroupMethod, _>(|_context, request: RpcFrame<Number>| async move {
            Ok(Ok(Total {
                value: request.view()?.value,
            }))
        })
        .expect("register other endpoint");

    let groups = registry.groups();
    assert_eq!(
        groups.iter().map(AsRef::as_ref).collect::<Vec<&str>>(),
        vec!["other", "typed"]
    );

    let typed = RpcGroup::try_from("typed").expect("valid group");
    let typed_rpcs = registry.rpcs(&typed);
    assert_eq!(
        typed_rpcs.iter().map(AsRef::as_ref).collect::<Vec<&str>>(),
        vec!["typed.frame_lease", "typed.unary_unary"]
    );

    let missing = RpcGroup::try_from("missing").expect("valid group");
    assert!(registry.rpcs(&missing).is_empty());

    registry
        .unregister(&unary)
        .expect("unregister unary endpoint");
    registry
        .unregister(&lease)
        .expect("unregister lease endpoint");
    assert_eq!(
        registry
            .groups()
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<&str>>(),
        vec!["other"]
    );

    assert_eq!(
        groups.iter().map(AsRef::as_ref).collect::<Vec<&str>>(),
        vec!["other", "typed"]
    );
}

#[test]
fn typed_unary_input_and_unary_output_are_self_driving() {
    let registry = registry();
    registry
        .register::<UnaryUnaryMethod, _>(|_context, request: RpcFrame<Number>| async move {
            Ok(Ok(Total {
                value: request.view()?.value + 1,
            }))
        })
        .expect("register typed endpoint");

    let call = registry
        .client()
        .call::<UnaryUnaryMethod>(number(41))
        .expect("start typed call");

    let response = block_on(call)
        .expect("complete unary call")
        .expect("method success");
    assert_eq!(response.view(), Ok(&Total { value: 42 }));
}

#[test]
fn typed_unary_input_and_stream_output_are_self_driving() {
    let registry = registry();
    registry
        .register::<UnaryStreamMethod, _>(|_context, request: RpcFrame<Number>| async move {
            let value = request.view()?.value;
            Ok(RpcStream::new(stream::iter(vec![
                Ok(Ok(Total { value })),
                Ok(Ok(Total { value: value + 1 })),
            ])))
        })
        .expect("register typed endpoint");

    let responses = registry
        .client()
        .call::<UnaryStreamMethod>(number(6))
        .expect("start typed call");

    assert_eq!(
        block_on(collect(responses)),
        Ok(vec![Total { value: 6 }, Total { value: 7 }])
    );
}

#[test]
fn typed_stream_method_error_is_terminal_and_zero_copy() {
    block_on(async {
        let registry = registry();
        registry
            .register::<UnaryStreamMethod, _>(|_context, _request: RpcFrame<Number>| async move {
                Ok(RpcStream::new(stream::iter([
                    Ok(Ok(Total { value: 1 })),
                    Ok(Err(MethodFailure { code: 7 })),
                    Ok(Ok(Total { value: 2 })),
                ])))
            })
            .expect("register typed endpoint");

        let mut responses = registry
            .client()
            .call::<UnaryStreamMethod>(number(0))
            .expect("start typed call");

        let first = responses
            .next()
            .await
            .expect("success response")
            .expect("transport success")
            .expect("method success");
        assert_eq!(first.view(), Ok(&Total { value: 1 }));
        drop(first);

        let failure = responses
            .next()
            .await
            .expect("method error response")
            .expect("transport success")
            .expect_err("typed method error");
        assert_eq!(failure.view(), Ok(&MethodFailure { code: 7 }));
        drop(failure);

        assert!(responses.next().await.is_none());
    });
}

#[test]
fn typed_stream_input_and_unary_output_are_self_driving() {
    let registry = registry();
    registry
        .register::<StreamUnaryMethod, _>(
            |_context, mut requests: RpcStream<RpcFrame<Number>>| async move {
                let mut total = 0_u32;
                while let Some(request) = requests.next().await {
                    let Some(next) = total.checked_add(request?.view()?.value) else {
                        return Ok(Err(MethodFailure { code: 1 }));
                    };
                    total = next;
                }
                Ok(Ok(Total { value: total }))
            },
        )
        .expect("register typed endpoint");

    let call = registry
        .client()
        .call::<StreamUnaryMethod>(input_stream([4, 5, 6]))
        .expect("start typed call");

    let response = block_on(call)
        .expect("complete stream-unary call")
        .expect("method success");
    assert_eq!(response.view(), Ok(&Total { value: 15 }));

    let failed = block_on(
        registry
            .client()
            .call::<StreamUnaryMethod>(input_stream([u32::MAX, 1]))
            .expect("start failing stream-unary call"),
    )
    .expect("complete failing stream-unary call")
    .expect_err("typed method failure");
    assert_eq!(failed.view(), Ok(&MethodFailure { code: 1 }));
}

#[test]
fn typed_stream_input_and_stream_output_are_self_driving() {
    let registry = registry();
    registry
        .register::<StreamStreamMethod, _>(
            |_context, requests: RpcStream<RpcFrame<Number>>| async move {
                let responses = stream::unfold(requests, |mut requests| async move {
                    match requests.next().await {
                        Some(Ok(request)) => match request.view() {
                            Ok(request) => Some((
                                Ok(Ok(Total {
                                    value: request.value + 10,
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
        )
        .expect("register typed endpoint");

    let responses = registry
        .client()
        .call::<StreamStreamMethod>(input_stream([1, 2, 3]))
        .expect("start typed call");

    assert_eq!(
        block_on(collect(responses)),
        Ok(vec![
            Total { value: 11 },
            Total { value: 12 },
            Total { value: 13 },
        ])
    );
}

#[test]
fn terminal_response_cancels_unconsumed_typed_input() {
    let registry = registry();
    registry
        .register::<StreamStreamMethod, _>(
            |_context, mut requests: RpcStream<RpcFrame<Number>>| async move {
                requests.next().await.expect("first request")?.view()?;
                Ok(RpcStream::new(stream::iter([Ok(Err(MethodFailure {
                    code: 9,
                }))])))
            },
        )
        .expect("register early-terminal endpoint");

    block_on(async {
        let mut responses = registry
            .client()
            .call::<StreamStreamMethod>(input_stream([1, 2, 3]))
            .expect("start typed call");

        let failure = responses
            .next()
            .await
            .expect("terminal response")
            .expect("transport success")
            .expect_err("method failure");
        assert_eq!(failure.view(), Ok(&MethodFailure { code: 9 }));
        drop(failure);
        assert!(responses.next().await.is_none());
    });
}

struct IncompatibleMethod;

impl RpcMethod for IncompatibleMethod {
    const ADDRESS: &'static str = UnaryUnaryMethod::ADDRESS;

    type Request = Number;
    type Response = Total;
    type Error = MethodFailure;
    type Input = Unary;
    type Output = Streaming;
}

#[test]
fn typed_signature_mismatch_is_rejected_before_starting_io() {
    let registry = registry();
    registry
        .register::<UnaryUnaryMethod, _>(|_context, request: RpcFrame<Number>| async move {
            Ok(Ok(Total {
                value: request.view()?.value,
            }))
        })
        .expect("register typed endpoint");

    let result = registry.client().call::<IncompatibleMethod>(number(1));

    assert!(matches!(
        result,
        Err(RpcError::SignatureMismatch { address, .. })
            if address.as_ref() == UnaryUnaryMethod::ADDRESS
    ));
}

struct EmptyFrameMethod;

impl RpcMethod for EmptyFrameMethod {
    const ADDRESS: &'static str = "typed.empty_frame";

    type Request = ();
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

#[test]
fn zero_byte_zerocopy_frames_are_preserved_by_static_lanes() {
    let registry = registry();
    registry
        .register::<EmptyFrameMethod, _>(|_context, request: RpcFrame<()>| async move {
            request.view()?;
            Ok(Ok(()))
        })
        .expect("register empty-frame endpoint");

    let call = registry
        .client()
        .call::<EmptyFrameMethod>(())
        .expect("start empty-frame call");
    let response = block_on(call)
        .expect("complete empty-frame call")
        .expect("method success");
    assert_eq!(response.view(), Ok(&()));
}

struct LeaseMethod;

impl RpcMethod for LeaseMethod {
    const ADDRESS: &'static str = "typed.frame_lease";

    type Request = Number;
    type Response = Number;
    type Error = MethodFailure;
    type Input = Unary;
    type Output = Unary;
}

#[test]
fn response_frame_retains_an_aligned_lane_until_drop() {
    block_on(async {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 64, 2>::new()));
        let registry = RpcRegistry::new(lanes);
        registry
            .register::<LeaseMethod, _>(|_context, request: RpcFrame<Number>| async move {
                Ok(Ok(*request.view()?))
            })
            .expect("register lease endpoint");

        let client = registry.client();
        let first = client
            .call::<LeaseMethod>(number(1))
            .expect("start first call")
            .await
            .expect("finish first call")
            .expect("method success");
        let address = first.view().expect("borrow typed response") as *const Number as usize;
        assert_eq!(address % align_of::<Number>(), 0);

        let mut second = Box::pin(
            client
                .call::<LeaseMethod>(number(2))
                .expect("start second call"),
        );
        assert!(poll_once(second.as_mut()).await.is_none());

        drop(first);
        let second = second
            .await
            .expect("finish second call after frame drop")
            .expect("method success");
        assert_eq!(second.view(), Ok(&number(2)));
    });
}

struct DirectSelfMethod;

impl RpcMethod for DirectSelfMethod {
    const ADDRESS: &'static str = "typed.direct_self";

    type Request = Number;
    type Response = Total;
    type Error = MethodFailure;
    type Input = Unary;
    type Output = Unary;
}

#[test]
fn typed_direct_self_call_is_rejected() {
    let registry = registry();
    registry
        .register::<DirectSelfMethod, _>(
            |context: RpcContext, request: RpcFrame<Number>| async move {
                let nested = context.client().call::<DirectSelfMethod>(*request.view()?);
                match nested {
                    Err(RpcError::DirectSelfCall(_)) => Ok(Ok(Total { value: 1 })),
                    Err(error) => Err(error),
                    Ok(_) => Ok(Err(MethodFailure { code: 2 })),
                }
            },
        )
        .expect("register self-call endpoint");

    let response = block_on(
        registry
            .client()
            .call::<DirectSelfMethod>(number(0))
            .expect("start outer call"),
    )
    .expect("complete outer call")
    .expect("method success");
    assert_eq!(response.view(), Ok(&Total { value: 1 }));
}

struct IndirectA;

impl RpcMethod for IndirectA {
    const ADDRESS: &'static str = "typed.indirect_a";

    type Request = Number;
    type Response = Total;
    type Error = MethodFailure;
    type Input = Unary;
    type Output = Unary;
}

struct IndirectB;

impl RpcMethod for IndirectB {
    const ADDRESS: &'static str = "typed.indirect_b";

    type Request = Number;
    type Response = Total;
    type Error = MethodFailure;
    type Input = Unary;
    type Output = Unary;
}

#[test]
fn typed_indirect_call_may_return_to_an_earlier_endpoint() {
    let registry = registry();
    registry
        .register::<IndirectA, _>(
            |context: RpcContext, request: RpcFrame<Number>| async move {
                let value = request.view()?.value;
                drop(request);
                if value == 0 {
                    let outcome = context.client().call::<IndirectB>(number(1))?.await?;
                    copy_outcome(outcome)
                } else {
                    Ok(Ok(Total { value }))
                }
            },
        )
        .expect("register endpoint A");
    registry
        .register::<IndirectB, _>(
            |context: RpcContext, request: RpcFrame<Number>| async move {
                let value = request.view()?.value;
                drop(request);
                let outcome = context
                    .client()
                    .call::<IndirectA>(number(value + 1))?
                    .await?;
                copy_outcome(outcome)
            },
        )
        .expect("register endpoint B");

    let response = block_on(
        registry
            .client()
            .call::<IndirectA>(number(0))
            .expect("start indirect call chain"),
    )
    .expect("complete indirect call chain")
    .expect("method success");
    assert_eq!(response.view(), Ok(&Total { value: 2 }));
}

#[test]
fn unregister_keeps_a_prepared_typed_call_alive() {
    let registry = registry();
    let registration = registry
        .register::<UnaryUnaryMethod, _>(|_context, request: RpcFrame<Number>| async move {
            Ok(Ok(Total {
                value: request.view()?.value + 1,
            }))
        })
        .expect("register typed endpoint");
    let client = registry.client();
    let in_flight = client
        .call::<UnaryUnaryMethod>(number(10))
        .expect("prepare typed call");

    registry
        .unregister(&registration)
        .expect("unregister exact endpoint");
    let response = block_on(in_flight)
        .expect("prepared call retains handler")
        .expect("method success");
    assert_eq!(response.view(), Ok(&Total { value: 11 }));
    drop(response);

    assert!(matches!(
        client.call::<UnaryUnaryMethod>(number(20)),
        Err(RpcError::NotFound(_))
    ));
    assert!(matches!(
        registry.unregister(&registration),
        Err(RpcError::StaleRegistration(_))
    ));
}

#[test]
fn dropping_a_reserved_typed_waiter_hands_the_lane_to_the_next_call() {
    block_on(async {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 64, 2>::new()));
        let registry = RpcRegistry::new(lanes);
        registry
            .register::<LeaseMethod, _>(|_context, request: RpcFrame<Number>| async move {
                Ok(Ok(*request.view()?))
            })
            .expect("register lease endpoint");
        let client = registry.client();

        let first = client
            .call::<LeaseMethod>(number(1))
            .expect("start first call")
            .await
            .expect("finish first call")
            .expect("method success");
        let mut second = Box::pin(
            client
                .call::<LeaseMethod>(number(2))
                .expect("start second call"),
        );
        let mut third = Box::pin(
            client
                .call::<LeaseMethod>(number(3))
                .expect("start third call"),
        );
        assert!(poll_once(second.as_mut()).await.is_none());
        assert!(poll_once(third.as_mut()).await.is_none());

        drop(first);
        drop(second);
        let third = third
            .await
            .expect("third call receives transferred lane")
            .expect("method success");
        assert_eq!(third.view(), Ok(&number(3)));
    });
}

struct InnerMethod;

impl RpcMethod for InnerMethod {
    const ADDRESS: &'static str = "typed.inner";

    type Request = Number;
    type Response = Total;
    type Error = MethodFailure;
    type Input = Unary;
    type Output = Unary;
}

struct OuterMethod;

impl RpcMethod for OuterMethod {
    const ADDRESS: &'static str = "typed.outer";

    type Request = Number;
    type Response = Total;
    type Error = MethodFailure;
    type Input = Unary;
    type Output = Unary;
}

#[test]
fn typed_nested_call_fails_instead_of_waiting_for_its_own_lane() {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 64, 1>::new()));
    let registry = RpcRegistry::new(lanes);
    registry
        .register::<InnerMethod, _>(|_context, request: RpcFrame<Number>| async move {
            Ok(Ok(Total {
                value: request.view()?.value,
            }))
        })
        .expect("register inner endpoint");
    registry
        .register::<OuterMethod, _>(
            |context: RpcContext, request: RpcFrame<Number>| async move {
                let request_value = *request.view()?;
                drop(request);
                let outcome = context.client().call::<InnerMethod>(request_value)?.await?;
                copy_outcome(outcome)
            },
        )
        .expect("register outer endpoint");

    let result = block_on(
        registry
            .client()
            .call::<OuterMethod>(number(1))
            .expect("start outer call"),
    );
    assert!(matches!(
        result,
        Err(RpcError::NestedLaneExhausted { limit: 1 })
    ));
}
