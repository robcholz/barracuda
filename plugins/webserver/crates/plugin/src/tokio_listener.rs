use alloc::boxed::Box;
use core::net::Ipv4Addr;

use barracuda_net::TokioStack;
use picoserve::io::{ErrorKind, ErrorType, Read, Socket, Write};
use picoserve::time::{Duration, TimeoutError, Timer};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::{WebServer, WebServerListenFuture, WebServerListener};

const HTTP_BUFFER_BYTES: usize = 8 * 1024;

struct TokioRuntime;

#[derive(Default)]
struct TokioTimer;

impl Timer<TokioRuntime> for TokioTimer {
    async fn delay(&self, duration: Duration) {
        tokio::time::sleep(std::time::Duration::from_millis(duration.as_millis())).await;
    }

    async fn run_with_timeout<F: core::future::Future>(
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

impl WebServerListener for TokioStack {
    type Error = std::io::Error;

    fn listen<'a>(
        &'a mut self,
        server: &'a WebServer,
        port: u16,
    ) -> WebServerListenFuture<'a, Self::Error> {
        Box::pin(async move {
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
            loop {
                let (socket, _remote_address) = listener.accept().await?;
                let mut http_buffer = [0_u8; HTTP_BUFFER_BYTES];
                let _result = server
                    .serve_connection(TokioTimer, &mut http_buffer, TokioSocket(socket))
                    .await;
            }
        })
    }
}
