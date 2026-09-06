//! Event Router must not obscure ownership of shared RPC constructors.

#[test]
fn event_router_does_not_reexport_the_inline_rpc_schema_constructor() {
    let facade = include_str!("../src/lib.rs");

    assert!(!facade.contains("json_schema_inline"));
}
