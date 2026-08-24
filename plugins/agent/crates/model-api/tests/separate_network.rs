//! TCP connection and DNS resolution are independent Model API inputs.

use core::net::{IpAddr, SocketAddr};

use barracuda_model_api::ModelApi;
use barracuda_platform_test::ScriptedStack;
use embedded_nal_async::{AddrType, Dns, TcpConnect};

struct TcpOnly<'a>(&'a ScriptedStack);

impl TcpConnect for TcpOnly<'_> {
    type Error = <ScriptedStack as TcpConnect>::Error;
    type Connection<'a>
        = <ScriptedStack as TcpConnect>::Connection<'a>
    where
        Self: 'a;

    async fn connect<'a>(
        &'a self,
        remote: SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        self.0.connect(remote).await
    }
}

struct ResolverOnly<'a>(&'a ScriptedStack);

impl Dns for ResolverOnly<'_> {
    type Error = <ScriptedStack as Dns>::Error;

    async fn get_host_by_name(
        &self,
        host: &str,
        addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        self.0.get_host_by_name(host, addr_type).await
    }

    async fn get_host_by_address(
        &self,
        address: IpAddr,
        result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        self.0.get_host_by_address(address, result).await
    }
}

#[test]
fn model_api_accepts_distinct_tcp_and_dns_implementations() {
    let stack = ScriptedStack::default();
    let tcp = TcpOnly(&stack);
    let resolver = ResolverOnly(&stack);

    let _api = ModelApi::new(&tcp, &resolver, 4096, 512);
}
