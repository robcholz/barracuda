use alloc::boxed::Box;
use core::convert::Infallible;

use crate::{WebServer, WebServerListenFuture, WebServerListener};

const HTTP_BUFFER_BYTES: usize = 8 * 1024;
const TCP_BUFFER_BYTES: usize = 4 * 1024;

impl WebServerListener for embassy_net::Stack<'static> {
    type Error = Infallible;

    fn listen<'a>(
        &'a mut self,
        server: &'a WebServer,
        port: u16,
    ) -> WebServerListenFuture<'a, Self::Error> {
        Box::pin(async move {
            let mut tcp_rx_buffer = [0_u8; TCP_BUFFER_BYTES];
            let mut tcp_tx_buffer = [0_u8; TCP_BUFFER_BYTES];
            loop {
                let mut socket =
                    embassy_net::tcp::TcpSocket::new(*self, &mut tcp_rx_buffer, &mut tcp_tx_buffer);
                if socket.accept(port).await.is_err() {
                    continue;
                }
                let mut http_buffer = [0_u8; HTTP_BUFFER_BYTES];
                let _result = server
                    .serve_connection(picoserve::time::EmbassyTimer, &mut http_buffer, socket)
                    .await;
            }
        })
    }
}
