//! Public construction context API tests.

use barracuda_platform_test::never_embassy_stack;
use barracuda_plugin_api::PluginContext;
use http_client::ClientFactory;

#[test]
fn construction_resources_are_public_fields() {
    let stack = never_embassy_stack();
    let context = PluginContext::new(stack, ClientFactory::plaintext(stack));

    let _ip_stack = context.ip_stack;
    let _http_clients = context.http_clients;
    let _hal = context.hal;
}
