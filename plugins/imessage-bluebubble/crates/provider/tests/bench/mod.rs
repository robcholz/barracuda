//! A real Gateway (beside Workflow, HTTP, and WebServer) on a loopback
//! network, whose HTTP the provider sends through, and a scripted
//! BlueBubbles server on the same network.

use std::{cell::RefCell, rc::Rc};

use barracuda_http_plugin::HttpPlugin;
use barracuda_imessage_gateway_plugin::{GatewayHttp, IMessageGateway, IMessageGatewayPlugin};
use barracuda_platform_test::{
    install_global_memory_vfs, loopback_network, memory_partition, ScriptStep, ScriptedServer,
};
use barracuda_plugin::api::{BoardInfo, Hardware, PlatformInfo, PluginContext, TargetIdentity};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_webserver_plugin::WebServerPlugin;
use barracuda_workflow_plugin::WorkflowPlugin;
use embassy_futures::select::{select3, Either3};
use futures_lite::future::block_on;

/// Port of the scripted BlueBubbles server on the loopback network.
const PORT: u16 = 9000;
/// URL of the scripted BlueBubbles server.
pub const SERVER_URL: &str = "http://10.0.0.1:9000";

/// Captures the Gateway capability during registration.
struct Probe(Rc<RefCell<Option<Rc<IMessageGateway>>>>);

impl PluginDeclaration for Probe {
    const ID: &'static str = "bluebubbles-provider-probe";
    const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway"];
}

impl Plugin for Probe {
    fn register<Storage: PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        self.0.replace(Some(
            context.require::<IMessageGateway>("imessage-gateway")?,
        ));
        Ok(())
    }
}

/// Runs `test` with the HTTP of a real Gateway on a loopback network, and a
/// BlueBubbles server there answering `steps` in order; a test pushes more
/// with [`ScriptedServer::push`]. A request that finds no step left has its
/// connection closed without an answer.
pub fn bench(
    steps: impl IntoIterator<Item = ScriptStep>,
    test: impl AsyncFnOnce(GatewayHttp, &ScriptedServer),
) {
    let network = loopback_network();
    let stack = network.stack();
    // Every BlueBubbles request opens its own connection.
    let server = ScriptedServer::new(steps);
    block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition = block_on(memory_partition(64 * 1024)).expect("test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
    let identity = TargetIdentity::new(
        PlatformInfo::new("test", "test", "test-arch", "hosted"),
        BoardInfo::new("test-board", Hardware::new("test-chip")),
    );
    let mut context = PluginContext::new(identity, stack);
    let gateway = Rc::new(RefCell::new(None));
    manager
        .register(WorkflowPlugin::new(&mut context))
        .expect("register Workflow");
    manager
        .register(HttpPlugin::new(&mut context))
        .expect("register HTTP");
    manager
        .register(WebServerPlugin::new(&mut context))
        .expect("register WebServer");
    manager
        .register(IMessageGatewayPlugin::new(&mut context))
        .expect("register Gateway");
    manager
        .register(Probe(Rc::clone(&gateway)))
        .expect("register probe");
    let http = gateway.take().expect("Gateway capability").http();
    // Each send opens a connection of its own and drops it without a FIN, so
    // it keeps one listener until the server's retransmission draws a reset;
    // the longest test sends five requests.
    let outcome = block_on(select3(
        network.run(),
        server.run::<8>(stack, PORT),
        test(http, &server),
    ));
    assert!(matches!(outcome, Either3::Third(())), "the bench stopped");
}
