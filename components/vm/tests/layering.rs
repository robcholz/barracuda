use barracuda_vm::{Result, Vm};

#[test]
fn vm_component_uses_the_shared_lua_wrapper() -> Result<()> {
    let mut vm = Vm::new()?;
    vm.lua_mut()
        .register("double", |value: i64| Some(Ok(value * 2)))?;

    assert_eq!(vm.lua_mut().load("return double(21)").eval::<i64>()?, 42);
    Ok(())
}
