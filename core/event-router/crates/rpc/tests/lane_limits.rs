#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use core::mem::size_of;

use barracuda_rpc::{
    RpcAddress, RpcError, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, Unary,
};
use futures_lite::future::{block_on, poll_once};

struct Echo;

impl RpcMethod for Echo {
    const ADDRESS: &'static str = "limits.echo";
    type Request = [u8; 8];
    type Response = [u8; 8];
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

fn registry<const N: usize, const M: usize, const Q: usize>() -> RpcRegistry<N, M, Q> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<N, M, Q>::new()));
    let registry = RpcRegistry::new(lanes);
    registry
        .register::<Echo, _>(|_context, request: RpcFrame<[u8; 8]>| async move {
            Ok(Ok(*request.view()?))
        })
        .expect("register echo endpoint");
    registry
}

#[test]
fn active_lanes_and_waiter_slots_hit_the_exact_configured_limits() {
    block_on(async {
        let registry = registry::<2, 8, 2>();
        let client = registry.client();

        let first = client
            .call::<Echo>(*b"first---")
            .expect("prepare first call")
            .await
            .expect("finish first call")
            .expect("first method success");
        let second = client
            .call::<Echo>(*b"second--")
            .expect("prepare second call")
            .await
            .expect("finish second call")
            .expect("second method success");

        let mut third = Box::pin(
            client
                .call::<Echo>(*b"third---")
                .expect("prepare third call"),
        );
        let mut fourth = Box::pin(
            client
                .call::<Echo>(*b"fourth--")
                .expect("prepare fourth call"),
        );
        let mut fifth = Box::pin(
            client
                .call::<Echo>(*b"fifth---")
                .expect("prepare fifth call"),
        );

        assert!(poll_once(third.as_mut()).await.is_none());
        assert!(poll_once(fourth.as_mut()).await.is_none());
        assert!(matches!(
            poll_once(fifth.as_mut()).await,
            Some(Err(RpcError::LaneWaiterCapacityExceeded { limit: 2 }))
        ));

        drop(first);
        let third = third
            .await
            .expect("third call receives first released lane")
            .expect("third method success");
        assert_eq!(third.view(), Ok(b"third---"));

        assert!(poll_once(fourth.as_mut()).await.is_none());
        drop(third);
        let fourth = fourth
            .await
            .expect("fourth call receives next released lane")
            .expect("fourth method success");
        assert_eq!(fourth.view(), Ok(b"fourth--"));

        drop(fourth);
        drop(second);
    });
}

#[test]
fn zero_waiter_capacity_rejects_a_saturated_root_call() {
    block_on(async {
        let registry = registry::<1, 8, 0>();
        let client = registry.client();
        let held = client
            .call::<Echo>(*b"held----")
            .expect("prepare held call")
            .await
            .expect("finish held call")
            .expect("held method success");

        let next = client
            .call::<Echo>(*b"next----")
            .expect("prepare saturated call");
        assert!(matches!(
            poll_once(next).await,
            Some(Err(RpcError::LaneWaiterCapacityExceeded { limit: 0 }))
        ));
        drop(held);
    });
}

#[test]
fn multicast_target_count_is_bounded_by_lane_count() {
    block_on(async {
        let registry = registry::<2, 8, 2>();
        let client = registry.client();
        let address = RpcAddress::try_from(Echo::ADDRESS).expect("valid echo address");

        let mut exact = client
            .multicast::<[u8; 8], Unary>(&[address.clone(), address.clone()], *b"exact---")
            .expect("prepare exact-size multicast");
        for branch in &mut exact {
            let response = branch
                .reader_mut()
                .read()
                .await
                .expect("read exact multicast branch")
                .expect("branch response")
                .expect("branch method success");
            assert_eq!(response.as_ref(), b"exact---");
        }
        drop(exact);

        let mut oversized = client
            .multicast::<[u8; 8], Unary>(&[address.clone(), address.clone(), address], *b"too-many")
            .expect("prepare oversized multicast");
        let result = oversized
            .first_mut()
            .expect("first branch")
            .reader_mut()
            .read()
            .await;
        assert!(matches!(
            result,
            Err(RpcError::LaneBatchTooLarge {
                requested: 3,
                limit: 2
            })
        ));
    });
}

#[test]
fn lane_storage_reserves_at_least_two_payload_frames_per_lane() {
    let one_lane = size_of::<RpcLaneStorage<1, 64, 1>>();
    let four_lanes = size_of::<RpcLaneStorage<4, 64, 8>>();
    let production_sized = size_of::<RpcLaneStorage<4, 4_096, 8>>();

    assert!(one_lane >= 2 * 64);
    assert!(four_lanes >= 4 * 2 * 64);
    assert!(production_sized >= 4 * 2 * 4_096);
    eprintln!(
        "lane_storage_bytes: N1_M64_Q1={one_lane}, N4_M64_Q8={four_lanes}, N4_M4096_Q8={production_sized}"
    );
}
