#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_event_router::{JsonRpcSchema, RpcAddress, RpcError, RpcLaneStorage, RpcRegistry};
use barracuda_vm_component::run::{Cancel, Input, Run, cancel_handler, input_handler, run_handler};
use barracuda_vm_component::{BuiltinPackages, VmLimits, VmRuntime};

fn registry() -> RpcRegistry<4, 512, 4> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
    let registry = RpcRegistry::new(lanes);
    let runtime = VmRuntime::new().expect("create runtime");
    let limits = VmLimits::default();
    let packages = barracuda_vm_package_api::LuaPackageRegistry::new();
    let _run = registry
        .register_json::<Run, _>(
            "*",
            run_handler(runtime.clone(), limits, BuiltinPackages::all(), packages),
        )
        .expect("register vm.run");
    let _input = registry
        .register_json::<Input, _>("*", input_handler(runtime.clone(), limits))
        .expect("register vm.input");
    let _cancel = registry
        .register_json::<Cancel, _>("*", cancel_handler(runtime))
        .expect("register vm.cancel");
    core::mem::forget((_run, _input, _cancel));
    registry
}

fn call(
    registry: &RpcRegistry<4, 512, 4>,
    method: &str,
    request: &str,
) -> Result<String, RpcError> {
    let address = RpcAddress::try_from(method).expect("valid address");
    let response = futures_lite::future::block_on(
        registry
            .client()
            .call_json(&address, request)
            .expect("start JSON call"),
    )?;
    Ok(response.as_str()?.to_owned())
}

#[test]
fn control_rpcs_are_public_and_return_stable_business_rejections() {
    let registry = registry();
    assert_eq!(
        registry
            .client()
            .rpcs_by_visibility("*")
            .expect("discover public RPCs"),
        [
            RpcAddress::try_from(Cancel::ADDRESS).expect("cancel address"),
            RpcAddress::try_from(Input::ADDRESS).expect("input address"),
            RpcAddress::try_from(Run::ADDRESS).expect("run address"),
        ]
    );
    assert!(
        registry
            .client()
            .rpcs_by_visibility("agent")
            .expect("query legacy visibility")
            .is_empty()
    );

    assert_eq!(
        call(&registry, Run::ADDRESS, r#"{"source":"return"}"#),
        Ok(r#"{"error":"runtime_unavailable"}"#.to_owned())
    );
    assert_eq!(
        call(&registry, Input::ADDRESS, r#"{"run_id":7,"input":"x"}"#),
        Ok(r#"{"error":"run_not_found"}"#.to_owned())
    );
    assert_eq!(
        call(&registry, Cancel::ADDRESS, r#"{"run_id":7}"#),
        Ok(r#"{"error":"run_not_found"}"#.to_owned())
    );
}

#[test]
fn malformed_shapes_are_transport_errors_and_source_limit_is_business_error() {
    let registry = registry();
    assert_eq!(
        call(&registry, Run::ADDRESS, r#"{"script":"return"}"#),
        Err(RpcError::InvalidJson)
    );
    assert_eq!(
        call(
            &registry,
            Input::ADDRESS,
            r#"{"run_id":7,"input":"x","eof":true}"#,
        ),
        Err(RpcError::InvalidJson)
    );

    let source = "x".repeat(VmLimits::default().max_source_bytes() + 1);
    let request = format!(r#"{{"source":"{source}"}}"#);
    assert_eq!(
        call(&registry, Run::ADDRESS, &request),
        Ok(r#"{"error":"source_limit_exceeded"}"#.to_owned())
    );
}
