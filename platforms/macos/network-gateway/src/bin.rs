//! Standalone Barracuda virtual-network gateway.

fn main() -> anyhow::Result<()> {
    barracuda_platform_macos_network_gateway::run_cli()
}
