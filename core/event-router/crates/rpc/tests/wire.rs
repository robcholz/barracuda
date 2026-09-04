#![allow(clippy::expect_used)]
#![allow(clippy::indexing_slicing)]
#![allow(missing_docs)]

use barracuda_rpc::{
    rpc_dynamic, RpcAddress, RpcError, RpcLaneStorage, RpcMethod, RpcRegistry, RpcWire, Unary,
};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

#[repr(C)]
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Copy,
    Debug,
    Default,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Eq,
    RpcWire,
    TryFromBytes,
)]
#[serde(rename_all = "camelCase")]
struct Reply {
    session_id: u32,
    digest: [u8; 8],
}

#[repr(C)]
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Copy,
    Debug,
    Default,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Eq,
    RpcWire,
    TryFromBytes,
)]
#[serde(rename_all = "camelCase")]
struct Deliver {
    #[serde(rename = "attachment")]
    payload: [u8; 8],
    session_id: u32,
}

struct Produce;

#[rpc_dynamic]
impl RpcMethod for Produce {
    const ADDRESS: &'static str = "wire.produce";
    type Request = Reply;
    type Response = Reply;
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

struct Consume;

#[rpc_dynamic]
impl RpcMethod for Consume {
    const ADDRESS: &'static str = "wire.consume";
    type Request = Deliver;
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

fn registry() -> RpcRegistry<2, 64, 2> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 64, 2>::new()));
    RpcRegistry::new(lanes)
}

fn register(registry: &RpcRegistry<2, 64, 2>) {
    registry
        .register::<Produce, _>("system", |_context, _request| async {
            Ok(Ok(Reply::default()))
        })
        .expect("register produce");
    registry
        .register::<Consume, _>("system", |_context, _request| async { Ok(Ok(())) })
        .expect("register consume");
}

#[test]
fn derive_honors_serde_rename_all_and_field_rename() {
    // rename_all = camelCase turns `session_id` into `sessionId`.
    assert!(Reply::FIELDS
        .iter()
        .any(|field| field.name() == "sessionId"));
    // field-level rename wins over the container convention.
    assert!(Deliver::FIELDS
        .iter()
        .any(|field| field.name() == "attachment"));
    assert!(!Deliver::FIELDS
        .iter()
        .any(|field| field.name() == "payload"));
}

#[test]
fn wire_reads_a_response_field_and_writes_it_into_a_request() {
    let registry = registry();
    register(&registry);
    let client = registry.client();

    let produce = client
        .method_info(&RpcAddress::try_from(Produce::ADDRESS).expect("address"))
        .expect("produce info");
    let consume = client
        .method_info(&RpcAddress::try_from(Consume::ADDRESS).expect("address"))
        .expect("consume info");

    let source = Reply {
        session_id: 9,
        digest: *b"ABCDEFGH",
    };
    let source_bytes = source.as_bytes();

    let produce_wire = produce.wire().expect("produce is dynamic");
    let consume_wire = consume.wire().expect("consume is dynamic");

    // Read the opaque digest region out of the response frame.
    let digest = produce_wire
        .read_response_field(source_bytes, "digest")
        .expect("read digest");
    assert_eq!(digest, b"ABCDEFGH");

    // Patch it into the (renamed) attachment field of a fresh request buffer.
    let mut request = vec![0_u8; consume.request_frame_size()];
    consume_wire
        .write_request_field(&mut request, "attachment", digest)
        .expect("write attachment");
    let delivered = Deliver::try_ref_from_bytes(&request).expect("valid request");
    assert_eq!(&delivered.payload, b"ABCDEFGH");
}

#[test]
fn write_field_rejects_oversize_and_unknown_fields() {
    let registry = registry();
    register(&registry);
    let consume = registry
        .client()
        .method_info(&RpcAddress::try_from(Consume::ADDRESS).expect("address"))
        .expect("consume info");
    let wire = consume.wire().expect("consume is dynamic");
    let mut request = vec![0_u8; consume.request_frame_size()];

    let too_large = wire.write_request_field(&mut request, "attachment", &[0_u8; 9]);
    assert!(matches!(
        too_large,
        Err(RpcError::WireFieldTooLarge { capacity: 8, .. })
    ));

    let unknown = wire.write_request_field(&mut request, "missing", &[0_u8; 1]);
    assert!(matches!(unknown, Err(RpcError::WireFieldUnknown { .. })));
}

#[test]
fn method_info_exposes_signature_type_identity() {
    let registry = registry();
    register(&registry);
    let client = registry.client();
    let produce = client
        .method_info(&RpcAddress::try_from(Produce::ADDRESS).expect("address"))
        .expect("produce info");
    let consume = client
        .method_info(&RpcAddress::try_from(Consume::ADDRESS).expect("address"))
        .expect("consume info");

    // Produce -> Produce would be a valid Direct link (Response == Request).
    assert_eq!(
        produce.descriptor().response_type_id(),
        produce.descriptor().request_type_id()
    );
    // Produce's Reply response is not Consume's Deliver request.
    assert_ne!(
        produce.descriptor().response_type_id(),
        consume.descriptor().request_type_id()
    );
}
