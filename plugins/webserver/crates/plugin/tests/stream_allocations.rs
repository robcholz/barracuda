//! Full picoserve request/response path with allocation-counted, bounded test IO.
#![allow(clippy::expect_used, clippy::panic)]
// The whole connection future is instantiated here with test IO; rustc 1.99
// needs more than the default 128 query depth to lay it out.
#![recursion_limit = "256"]

use core::future::Future;
use core::task::{Context, Poll, Waker};

use barracuda_webserver_plugin::{HttpProvider, HttpResponse, WebServer};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use picoserve::io::Socket;
use picoserve::time::{Duration, TimeoutError, Timer};

struct Assets;

impl HttpProvider for Assets {
    async fn serve(&self, path: &str) -> HttpResponse {
        assert_eq!(path, "/assets/a%20b.js");
        HttpResponse::stream(200, "application/javascript", 40_000, Source(40_000))
    }
}

struct Source(usize);

impl ErrorType for Source {
    type Error = ErrorKind;
}

impl Read for Source {
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, ErrorKind> {
        yield_once().await;
        assert!(bytes.len() <= 1024);
        let count = self.0.min(bytes.len()).min(333);
        bytes[..count].fill(b'x');
        self.0 -= count;
        Ok(count)
    }
}

async fn yield_once() {
    let mut yielded = false;
    core::future::poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

struct Input(&'static [u8]);

impl ErrorType for Input {
    type Error = ErrorKind;
}

impl Read for Input {
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, ErrorKind> {
        let count = bytes.len().min(self.0.len());
        bytes[..count].copy_from_slice(&self.0[..count]);
        self.0 = &self.0[count..];
        Ok(count)
    }
}

struct Output {
    bytes: [u8; 41_000],
    length: usize,
    fail_after: usize,
}

impl ErrorType for Output {
    type Error = ErrorKind;
}

impl Write for Output {
    async fn write(&mut self, bytes: &[u8]) -> Result<usize, ErrorKind> {
        yield_once().await;
        if self.length >= self.fail_after {
            return Err(ErrorKind::BrokenPipe);
        }
        let count = bytes.len().min(97);
        self.bytes[self.length..self.length + count].copy_from_slice(&bytes[..count]);
        self.length += count;
        Ok(count)
    }

    async fn flush(&mut self) -> Result<(), ErrorKind> {
        Ok(())
    }
}

impl picoserve::io::Write for Output {
    async fn write_with<F: FnOnce(&mut [u8]) -> (usize, R), R>(
        &mut self,
        f: F,
    ) -> Result<R, ErrorKind> {
        let mut scratch = [0; 128];
        let (count, result) = f(&mut scratch);
        self.write_all(&scratch[..count]).await?;
        Ok(result)
    }
}

struct TestSocket<'a> {
    input: Input,
    output: &'a mut Output,
}

impl Socket<()> for TestSocket<'_> {
    type Error = ErrorKind;
    type ReadHalf<'a>
        = &'a mut Input
    where
        Self: 'a;
    type WriteHalf<'a>
        = &'a mut Output
    where
        Self: 'a;

    fn split(&mut self) -> (Self::ReadHalf<'_>, Self::WriteHalf<'_>) {
        (&mut self.input, self.output)
    }

    async fn abort<T: Timer<()>>(
        self,
        _: &picoserve::Timeouts,
        _: &mut T,
    ) -> Result<(), picoserve::Error<ErrorKind>> {
        Ok(())
    }

    async fn shutdown<T: Timer<()>>(
        self,
        _: &picoserve::Timeouts,
        _: &mut T,
    ) -> Result<(), picoserve::Error<ErrorKind>> {
        Ok(())
    }
}

struct TestTimer;

impl Timer<()> for TestTimer {
    async fn delay(&self, _: Duration) {
        core::future::pending().await
    }

    async fn run_with_timeout<F: Future>(
        &self,
        _: Duration,
        future: F,
    ) -> Result<F::Output, TimeoutError> {
        Ok(future.await)
    }
}

fn drive<F: Future>(future: F) -> F::Output {
    let mut future = core::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..100_000 {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return result;
        }
    }
    panic!("request did not complete");
}

#[test]
fn full_streaming_request_allocates_nothing_even_with_backpressure() {
    let server = WebServer::new();
    let _route = server.serve("/assets/*", Assets).expect("register");
    let mut output = Output {
        bytes: [0; 41_000],
        length: 0,
        fail_after: usize::MAX,
    };
    let mut buffer = [0; 4096];
    let allocations = allocation_counter::measure(|| {
        drive(server.serve_connection(TestTimer, &mut buffer, TestSocket {
            input: Input(b"GET /assets/a%20b.js?v=1 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"),
            output: &mut output,
        })).expect("serve");
    });
    assert_eq!(allocations.count_total, 0, "{allocations:?}");
    let bytes = &output.bytes[..output.length];
    let boundary = bytes
        .windows(4)
        .position(|p| p == b"\r\n\r\n")
        .expect("headers")
        + 4;
    assert!(bytes.starts_with(b"HTTP/1.1 200"));
    assert_eq!(bytes.len() - boundary, 40_000);
    assert!(bytes[boundary..].iter().all(|byte| *byte == b'x'));
}

#[test]
fn resource_routes_reject_other_methods_and_do_not_match_sibling_prefixes() {
    let server = WebServer::new();
    let route = server.serve("/assets/*", Assets).expect("register");
    assert!(server.serve("/assets/*", Assets).is_err());
    assert!(server.serve("/as*sets/*", Assets).is_err());
    for (request, status) in [
        (
            &b"POST /assets/a.js HTTP/1.1\r\nHost: localhost\r\n\r\n"[..],
            &b"HTTP/1.1 405"[..],
        ),
        (
            &b"GET /assets-other/a.js HTTP/1.1\r\nHost: localhost\r\n\r\n"[..],
            &b"HTTP/1.1 404"[..],
        ),
    ] {
        let mut output = Output {
            bytes: [0; 41_000],
            length: 0,
            fail_after: usize::MAX,
        };
        drive(server.serve_connection(
            TestTimer,
            &mut [0; 4096],
            TestSocket {
                input: Input(request),
                output: &mut output,
            },
        ))
        .expect("serve rejection");
        assert!(output.bytes[..output.length].starts_with(status));
    }
    drop(route);
    let _replacement = server
        .serve("/assets/*", Assets)
        .expect("released registration");
}

#[test]
fn socket_failure_stops_a_stream() {
    let server = WebServer::new();
    let _route = server.serve("/assets/*", Assets).expect("register");
    let mut output = Output {
        bytes: [0; 41_000],
        length: 0,
        fail_after: 1024,
    };
    let result = drive(server.serve_connection(
        TestTimer,
        &mut [0; 4096],
        TestSocket {
            input: Input(b"GET /assets/a%20b.js HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            output: &mut output,
        },
    ));
    assert!(result.is_err());
    assert!(output.length < 2048);
}

#[test]
fn cancelling_pending_response_drops_its_resource() {
    use std::cell::Cell;
    use std::rc::Rc;

    struct PendingSource(Rc<Cell<bool>>, Rc<Cell<bool>>);
    impl Drop for PendingSource {
        fn drop(&mut self) {
            self.1.set(true);
        }
    }
    impl ErrorType for PendingSource {
        type Error = ErrorKind;
    }
    impl Read for PendingSource {
        async fn read(&mut self, _: &mut [u8]) -> Result<usize, ErrorKind> {
            self.0.set(true);
            core::future::pending().await
        }
    }
    struct PendingProvider(Rc<Cell<bool>>, Rc<Cell<bool>>);
    impl HttpProvider for PendingProvider {
        async fn serve(&self, _: &str) -> HttpResponse {
            HttpResponse::stream(
                200,
                "text/plain",
                1,
                PendingSource(self.0.clone(), self.1.clone()),
            )
        }
    }

    let reading = Rc::new(Cell::new(false));
    let dropped = Rc::new(Cell::new(false));
    let server = WebServer::new();
    let _route = server
        .serve("/*", PendingProvider(reading.clone(), dropped.clone()))
        .expect("register");
    let mut output = Output {
        bytes: [0; 41_000],
        length: 0,
        fail_after: usize::MAX,
    };
    let mut buffer = [0; 4096];
    {
        let mut request = core::pin::pin!(server.serve_connection(
            TestTimer,
            &mut buffer,
            TestSocket {
                input: Input(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n"),
                output: &mut output,
            }
        ));
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..1000 {
            assert!(request.as_mut().poll(&mut cx).is_pending());
            if reading.get() {
                break;
            }
        }
        assert!(reading.get());
        assert!(!dropped.get());
    }
    assert!(dropped.get());
}
