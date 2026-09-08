//! Standalone Barracuda virtual-network gateway.

fn main() -> anyhow::Result<()> {
    barracuda_platform_net_gateway::run_cli()
}
