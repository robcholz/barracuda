use alloc::rc::Rc;

use barracuda_bulk_memory::BulkBox;
use barracuda_plugin::manager::PluginTaskToken;
use embassy_futures::join::{join, join_array};
use embassy_futures::select::select;
use embassy_net::{tcp::TcpSocket, Stack};
use picoserve::time::EmbassyTimer;

use crate::{WebListener, WebServer, WEB_SERVER_CONNECTION_SLOTS, WEB_SERVER_PORT};

const TCP_BUFFER_BYTES: usize = 4 * 1024;
const HTTP_BUFFER_BYTES: usize = 8 * 1024;

#[embassy_executor::task]
pub(crate) async fn web_server(
    webserver: Rc<WebServer>,
    stack: Stack<'static>,
    cancellation: PluginTaskToken,
) {
    let serving = serve_on_stack::<WEB_SERVER_CONNECTION_SLOTS>(webserver, stack, WEB_SERVER_PORT);
    let _completed = select(cancellation.cancelled(), serving).await;
    log::info!("stopped WebServer task");
}

#[embassy_executor::task]
pub(crate) async fn additional_web_server(
    webserver: Rc<WebServer>,
    listener: WebListener,
    cancellation: PluginTaskToken,
) {
    let stopped = select(cancellation.cancelled(), listener.stopped.wait());
    let serving = serve_on_stack::<1>(webserver, listener.stack, listener.port);
    let _completed = select(stopped, serving).await;
    log::info!("stopped additional WebServer listener");
}

async fn serve_on_stack<const CONNECTION_SLOTS: usize>(
    webserver: Rc<WebServer>,
    stack: Stack<'static>,
    port: u16,
) {
    log::info!("starting WebServer on port {port} with {CONNECTION_SLOTS} connection workers");
    let workers: [_; CONNECTION_SLOTS] =
        core::array::from_fn(|worker| serve_worker(Rc::clone(&webserver), stack, port, worker));
    join(log_listening_url(stack, port), join_array(workers)).await;
}

async fn log_listening_url(stack: Stack<'static>, port: u16) {
    stack.wait_config_up().await;
    if let Some(config) = stack.config_v4() {
        log::info!(
            "WebServer listening at http://{}:{port}/",
            config.address.address()
        );
    }
}

async fn serve_worker(webserver: Rc<WebServer>, stack: Stack<'static>, port: u16, worker: usize) {
    let mut tcp_rx_buffer = BulkBox::<[u8]>::new_zeroed_slice(TCP_BUFFER_BYTES);
    let mut tcp_tx_buffer = BulkBox::<[u8]>::new_zeroed_slice(TCP_BUFFER_BYTES);
    let mut http_buffer = BulkBox::<[u8]>::new_zeroed_slice(HTTP_BUFFER_BYTES);

    loop {
        let mut socket = TcpSocket::new(stack, &mut tcp_rx_buffer[..], &mut tcp_tx_buffer[..]);
        if let Err(error) = socket.accept(port).await {
            log::warn!("WebServer worker {worker} accept failed: {error:?}");
            continue;
        }
        let remote = socket.remote_endpoint();
        log::debug!("WebServer worker {worker} accepted connection from {remote:?}");
        match webserver
            .serve_connection(EmbassyTimer, &mut http_buffer[..], socket)
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
