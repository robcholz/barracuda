//! Public construction context API tests.

use barracuda_platform_test::never_embassy_stack;
use barracuda_plugin_api::{BoardInfo, Hardware, PlatformInfo, PluginContext, TargetIdentity};
use http_client::ClientFactory;

#[test]
fn construction_resources_are_public_fields() {
    let stack = never_embassy_stack();
    let identity = TargetIdentity::new(
        PlatformInfo::new("test", "test", "test-arch", "hosted"),
        BoardInfo::new("test-board", Hardware::new("test-chip")),
    );
    let context = PluginContext::new(identity, stack, ClientFactory::plaintext(stack));

    let _target_identity = context.target_identity;
    let _ip_stack = context.ip_stack;
    let _http_clients = context.http_clients;
    let _hal = context.hal;
}
