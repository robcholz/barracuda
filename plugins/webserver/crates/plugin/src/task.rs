use alloc::rc::Rc;

use embassy_futures::join::join_array;
use embassy_net::{tcp::TcpSocket, Stack};
use picoserve::time::EmbassyTimer;

use crate::{WebServer, WEB_SERVER_CONNECTION_SLOTS, WEB_SERVER_PORT};

const TCP_BUFFER_BYTES: usize = 4 * 1024;
const HTTP_BUFFER_BYTES: usize = 8 * 1024;

#[embassy_executor::task]
pub(crate) async fn web_server(webserver: Rc<WebServer>, stack: Stack<'static>) {
    log::info!(
        "starting WebServer on port {WEB_SERVER_PORT} with {WEB_SERVER_CONNECTION_SLOTS} connection workers"
    );
    let workers: [_; WEB_SERVER_CONNECTION_SLOTS] =
        core::array::from_fn(|worker| serve_worker(Rc::clone(&webserver), stack, worker));
    join_array(workers).await;
}

async fn serve_worker(webserver: Rc<WebServer>, stack: Stack<'static>, worker: usize) {
    let mut tcp_rx_buffer = [0_u8; TCP_BUFFER_BYTES];
    let mut tcp_tx_buffer = [0_u8; TCP_BUFFER_BYTES];
    let mut http_buffer = [0_u8; HTTP_BUFFER_BYTES];

    loop {
        let mut socket = TcpSocket::new(stack, &mut tcp_rx_buffer, &mut tcp_tx_buffer);
        if let Err(error) = socket.accept(WEB_SERVER_PORT).await {
            log::warn!("WebServer worker {worker} accept failed: {error:?}");
            continue;
        }
        let remote = socket.remote_endpoint();
        log::debug!("WebServer worker {worker} accepted connection from {remote:?}");
        match webserver
            .serve_connection(EmbassyTimer, &mut http_buffer, socket)
            .await
        {
            Ok(disconnection) => log::debug!(
                "WebServer worker {worker} closed connection from {remote:?} after {} request(s)",
                disconnection.handled_requests_count
            ),
            Err(error) => {
                log::warn!("WebServer worker {worker} connection from {remote:?} failed: {error}");
            }
        }
    }
}
