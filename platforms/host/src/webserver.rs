//! Host listener adapter for the portable WebServer.

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::future::Future;
use core::net::Ipv4Addr;
#[cfg(any(test, feature = "testing"))]
use core::pin::Pin;

use crate::TokioStack;
use barracuda_webserver_plugin::{
    WebServer, WebServerListenFuture, WebServerListener, WEB_SERVER_CONNECTION_SLOTS,
};
use embassy_executor::{SpawnError, Spawner};
use futures_util::{
    future::{select, Either},
    pin_mut,
};
#[cfg(any(test, feature = "testing"))]
use futures_util::{stream::FuturesUnordered, StreamExt as _};
use picoserve::io::{ErrorKind, ErrorType, Read, Socket, Write};
use picoserve::time::{Duration, TimeoutError, Timer};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const HTTP_BUFFER_BYTES: usize = 8 * 1024;

struct TokioRuntime;

#[derive(Default)]
struct TokioTimer;

impl Timer<TokioRuntime> for TokioTimer {
    async fn delay(&self, duration: Duration) {
        tokio::time::sleep(std::time::Duration::from_millis(duration.as_millis())).await;
    }

    async fn run_with_timeout<F: Future>(
        &self,
        duration: Duration,
        future: F,
    ) -> Result<F::Output, TimeoutError> {
        tokio::time::timeout(
            std::time::Duration::from_millis(duration.as_millis()),
            future,
        )
        .await
        .map_err(|_elapsed| TimeoutError)
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
struct TokioIoError(std::io::Error);

impl picoserve::io::Error for TokioIoError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

struct TokioIo<T>(T);

impl<T> ErrorType for TokioIo<T> {
    type Error = TokioIoError;
}

impl<T> Read for TokioIo<T>
where
    T: tokio::io::AsyncRead + Unpin,
{
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.read(buffer).await.map_err(TokioIoError)
    }
}

impl<T> Write for TokioIo<T>
where
    T: tokio::io::AsyncWrite + Unpin,
{
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.0.write(buffer).await.map_err(TokioIoError)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush().await.map_err(TokioIoError)
    }
}

struct TokioSocket(tokio::net::TcpStream);

impl Socket<TokioRuntime> for TokioSocket {
    type Error = TokioIoError;
    type ReadHalf<'a> = TokioIo<tokio::net::tcp::ReadHalf<'a>>;
    type WriteHalf<'a> = TokioIo<tokio::net::tcp::WriteHalf<'a>>;

    fn split(&mut self) -> (Self::ReadHalf<'_>, Self::WriteHalf<'_>) {
        let (read, write) = self.0.split();
        (TokioIo(read), TokioIo(write))
    }

    async fn abort<T>(
        self,
        _timeouts: &picoserve::Timeouts,
        _timer: &mut T,
    ) -> Result<(), picoserve::Error<Self::Error>>
    where
        T: Timer<TokioRuntime>,
    {
        Ok(())
    }

    async fn shutdown<T>(
        mut self,
        timeouts: &picoserve::Timeouts,
        timer: &mut T,
    ) -> Result<(), picoserve::Error<Self::Error>>
    where
        T: Timer<TokioRuntime>,
    {
        timer
            .run_with_timeout(timeouts.write, self.0.shutdown())
            .await
            .map_err(picoserve::Error::WriteTimeout)?
            .map_err(|error| picoserve::Error::Write(TokioIoError(error)))?;

        let mut buffer = [0_u8; 128];
        while timer
            .run_with_timeout(timeouts.read_request, self.0.read(&mut buffer))
            .await
            .map_err(picoserve::Error::ReadTimeout)?
            .map_err(|error| picoserve::Error::Read(TokioIoError(error)))?
            > 0
        {}
        Ok(())
    }
}

#[cfg(any(test, feature = "testing"))]
type ConnectionFuture<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

async fn serve_connection(server: &WebServer, socket: tokio::net::TcpStream) {
    let mut http_buffer = [0_u8; HTTP_BUFFER_BYTES];
    let _result = server
        .serve_connection(TokioTimer, &mut http_buffer, TokioSocket(socket))
        .await;
}

#[embassy_executor::task(pool_size = WEB_SERVER_CONNECTION_SLOTS)]
async fn connection_task(
    server: Rc<WebServer>,
    socket: tokio::net::TcpStream,
    completed: async_channel::Sender<()>,
) {
    serve_connection(&server, socket).await;
    let _result = completed.send(()).await;
}

fn spawn_connection(
    spawner: Spawner,
    server: &Rc<WebServer>,
    socket: tokio::net::TcpStream,
    completed: &async_channel::Sender<()>,
) -> Result<(), std::io::Error> {
    spawner
        .spawn(connection_task(
            Rc::clone(server),
            socket,
            completed.clone(),
        ))
        .map_err(|SpawnError::Busy| {
            std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "WebServer Embassy connection task pool is full",
            )
        })
}

async fn listen_with_embassy_pool(
    spawner: Spawner,
    server: Rc<WebServer>,
    port: u16,
    connection_slots: usize,
) -> Result<(), std::io::Error> {
    if connection_slots != WEB_SERVER_CONNECTION_SLOTS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "WebServer connection slots must match its Embassy task pool size",
        ));
    }

    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let (completed_tx, completed_rx) = async_channel::bounded(connection_slots);
    let mut active = 0_usize;

    loop {
        if active >= connection_slots {
            completed_rx.recv().await.map_err(|_closed| {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "WebServer connection completion channel closed",
                )
            })?;
            active = active.saturating_sub(1);
            continue;
        }

        if active == 0 {
            let (socket, _remote_address) = listener.accept().await?;
            spawn_connection(spawner, &server, socket, &completed_tx)?;
            active = active.saturating_add(1);
            continue;
        }

        let event = {
            let accepted = listener.accept();
            let completed = completed_rx.recv();
            pin_mut!(accepted, completed);
            match select(accepted, completed).await {
                Either::Left((result, _remaining_completed)) => Either::Left(result),
                Either::Right((result, _remaining_accepted)) => Either::Right(result),
            }
        };
        match event {
            Either::Left(result) => {
                let (socket, _remote_address) = result?;
                spawn_connection(spawner, &server, socket, &completed_tx)?;
                active = active.saturating_add(1);
            }
            Either::Right(result) => {
                result.map_err(|_closed| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "WebServer connection completion channel closed",
                    )
                })?;
                active = active.saturating_sub(1);
            }
        }
    }
}

#[cfg(any(test, feature = "testing"))]
async fn listen_cooperatively(
    server: Rc<WebServer>,
    port: u16,
    connection_slots: usize,
) -> Result<(), std::io::Error> {
    if connection_slots == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "WebServer connection pool must contain at least one slot",
        ));
    }

    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let mut active = FuturesUnordered::<ConnectionFuture<'_>>::new();

    loop {
        if active.len() >= connection_slots {
            let _completed = active.next().await;
            continue;
        }

        if active.is_empty() {
            let (socket, _remote_address) = listener.accept().await?;
            active.push(Box::pin(serve_connection(&server, socket)));
            continue;
        }

        let event = {
            let accepted = listener.accept();
            let completed = active.next();
            pin_mut!(accepted, completed);
            match select(accepted, completed).await {
                Either::Left((result, _remaining_completed)) => Either::Left(result),
                Either::Right((result, _remaining_accepted)) => Either::Right(result),
            }
        };
        match event {
            Either::Left(result) => {
                let (socket, _remote_address) = result?;
                active.push(Box::pin(serve_connection(&server, socket)));
            }
            Either::Right(_completed) => {}
        }
    }
}

impl WebServerListener for TokioStack {
    type Error = std::io::Error;

    fn listen<'a>(
        &'a mut self,
        server: Rc<WebServer>,
        port: u16,
        connection_slots: usize,
    ) -> WebServerListenFuture<'a, Self::Error> {
        Box::pin(async move {
            if let Some(spawner) = self.spawner() {
                listen_with_embassy_pool(spawner, server, port, connection_slots).await
            } else {
                #[cfg(any(test, feature = "testing"))]
                {
                    listen_cooperatively(server, port, connection_slots).await
                }
                #[cfg(not(any(test, feature = "testing")))]
                {
                    let _server = server;
                    let _port = port;
                    let _connection_slots = connection_slots;
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "Host WebServer requires normal Platform initialization with an Embassy spawner",
                    ))
                }
            }
        })
    }
}
