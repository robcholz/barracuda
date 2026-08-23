use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::poll_fn;
use core::net::{IpAddr, Ipv4Addr, SocketAddr};
use core::task::Poll;

use embedded_io::{Error, ErrorKind, ErrorType};
use embedded_io_async::{Read, Write};
use embedded_nal_async::{AddrType, ConnectedUdp, Dns, TcpConnect, UdpStack, UnconnectedUdp};

#[derive(Clone, Copy, Debug)]
pub struct ScriptError(pub ErrorKind);

impl core::fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "scripted network error: {:?}", self.0)
    }
}

impl core::error::Error for ScriptError {}

impl Error for ScriptError {
    fn kind(&self) -> ErrorKind {
        self.0
    }
}

#[derive(Clone, Debug)]
pub enum ScriptStep {
    Response {
        bytes: Vec<u8>,
        max_read: usize,
        pending_after: bool,
    },
    ConnectError(ErrorKind),
}

impl ScriptStep {
    #[must_use]
    pub fn json(status: u16, body: &str) -> Self {
        Self::response(status, "application/json", body.as_bytes(), usize::MAX)
    }

    #[must_use]
    pub fn sse(status: u16, chunks: &[&str]) -> Self {
        let body = chunks.concat();
        Self::response(status, "text/event-stream", body.as_bytes(), 7)
    }

    #[must_use]
    pub fn response(status: u16, content_type: &str, body: &[u8], max_read: usize) -> Self {
        let reason = if (200..300).contains(&status) {
            "OK"
        } else {
            "Error"
        };
        let head = alloc::format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
            body.len()
        );
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(body);
        Self::Response {
            bytes,
            max_read: max_read.max(1),
            pending_after: false,
        }
    }

    #[must_use]
    pub fn pending_after_headers(status: u16, content_type: &str) -> Self {
        let bytes = alloc::format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n"
        )
        .into_bytes();
        Self::Response {
            bytes,
            max_read: usize::MAX,
            pending_after: true,
        }
    }
}

#[derive(Default)]
struct State {
    steps: VecDeque<ScriptStep>,
    requests: Vec<Vec<u8>>,
    connect_count: usize,
}

#[derive(Clone, Default)]
pub struct ScriptedStack {
    state: Rc<RefCell<State>>,
}

impl ScriptedStack {
    #[must_use]
    pub fn new(steps: impl IntoIterator<Item = ScriptStep>) -> Self {
        Self {
            state: Rc::new(RefCell::new(State {
                steps: steps.into_iter().collect(),
                requests: Vec::new(),
                connect_count: 0,
            })),
        }
    }

    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.state
            .borrow()
            .requests
            .iter()
            .map(|request| String::from_utf8_lossy(request).to_string())
            .collect()
    }

    #[must_use]
    pub fn remaining(&self) -> usize {
        self.state.borrow().steps.len()
    }

    #[must_use]
    pub fn connect_count(&self) -> usize {
        self.state.borrow().connect_count
    }
}

impl Dns for ScriptedStack {
    type Error = ScriptError;

    async fn get_host_by_name(
        &self,
        _host: &str,
        _addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        Ok(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        Err(ScriptError(ErrorKind::Unsupported))
    }
}

impl TcpConnect for ScriptedStack {
    type Error = ScriptError;
    type Connection<'a> = ScriptedConnection;

    async fn connect<'a>(
        &'a self,
        _remote: SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        let connect_error = {
            let mut state = self.state.borrow_mut();
            state.connect_count += 1;
            match state.steps.front() {
                Some(ScriptStep::ConnectError(_)) => match state.steps.pop_front() {
                    Some(ScriptStep::ConnectError(kind)) => Some(kind),
                    _ => None,
                },
                Some(ScriptStep::Response { .. }) => None,
                None => return Err(ScriptError(ErrorKind::NotConnected)),
            }
        };
        if let Some(kind) = connect_error {
            Err(ScriptError(kind))
        } else {
            Ok(ScriptedConnection {
                state: Rc::clone(&self.state),
                request_index: None,
                response: Vec::new(),
                response_offset: 0,
                max_read: usize::MAX,
                pending_after: false,
            })
        }
    }
}

pub struct ScriptedConnection {
    state: Rc<RefCell<State>>,
    request_index: Option<usize>,
    response: Vec<u8>,
    response_offset: usize,
    max_read: usize,
    pending_after: bool,
}

impl ErrorType for ScriptedConnection {
    type Error = ScriptError;
}

impl Read for ScriptedConnection {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        if self.response_offset == self.response.len() && self.pending_after {
            return poll_fn(|_| Poll::Pending).await;
        }
        let remaining = &self.response[self.response_offset..];
        let count = remaining.len().min(buffer.len()).min(self.max_read);
        buffer[..count].copy_from_slice(&remaining[..count]);
        self.response_offset += count;
        Ok(count)
    }
}

impl Write for ScriptedConnection {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        if !self.response.is_empty() && self.response_offset == self.response.len() {
            self.response.clear();
            self.response_offset = 0;
            self.request_index = None;
        }
        let request_index = match self.request_index {
            Some(index) => index,
            None => {
                let mut state = self.state.borrow_mut();
                state.requests.push(Vec::new());
                let index = state.requests.len() - 1;
                self.request_index = Some(index);
                index
            }
        };
        self.state.borrow_mut().requests[request_index].extend_from_slice(buffer);
        Ok(buffer.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        if !self.response.is_empty() {
            return Ok(());
        }
        let step = self
            .state
            .borrow_mut()
            .steps
            .pop_front()
            .ok_or(ScriptError(ErrorKind::NotConnected))?;
        match step {
            ScriptStep::Response {
                bytes,
                max_read,
                pending_after,
            } => {
                self.response = bytes;
                self.max_read = max_read;
                self.pending_after = pending_after;
                Ok(())
            }
            ScriptStep::ConnectError(kind) => Err(ScriptError(kind)),
        }
    }
}

/// A zero-sized network stack for ownership/concurrency tests that never issue
/// a request. Unlike [`ScriptedStack`], it can live in a real `static`, so host
/// tests exercise the same borrowed-static client model as firmware.
pub struct NeverStack;

impl Dns for NeverStack {
    type Error = ScriptError;

    async fn get_host_by_name(
        &self,
        _host: &str,
        _addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }
}

impl TcpConnect for NeverStack {
    type Error = ScriptError;
    type Connection<'a> = NeverConnection;

    async fn connect<'a>(
        &'a self,
        _remote: SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }
}

impl UdpStack for NeverStack {
    type Error = ScriptError;
    type Connected = NeverUdp;
    type UniquelyBound = NeverUdp;
    type MultiplyBound = NeverUdp;

    async fn connect_from(
        &self,
        _local: SocketAddr,
        _remote: SocketAddr,
    ) -> Result<(SocketAddr, Self::Connected), Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }

    async fn bind_single(
        &self,
        _local: SocketAddr,
    ) -> Result<(SocketAddr, Self::UniquelyBound), Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }

    async fn bind_multiple(&self, _local: SocketAddr) -> Result<Self::MultiplyBound, Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }
}

/// UDP socket used by [`NeverStack`].
pub struct NeverUdp;

impl ConnectedUdp for NeverUdp {
    type Error = ScriptError;

    async fn send(&mut self, _data: &[u8]) -> Result<(), Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }

    async fn receive_into(&mut self, _buffer: &mut [u8]) -> Result<usize, Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }
}

impl UnconnectedUdp for NeverUdp {
    type Error = ScriptError;

    async fn send(
        &mut self,
        _local: SocketAddr,
        _remote: SocketAddr,
        _data: &[u8],
    ) -> Result<(), Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }

    async fn receive_into(
        &mut self,
        _buffer: &mut [u8],
    ) -> Result<(usize, SocketAddr, SocketAddr), Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }
}

pub struct NeverConnection;

impl ErrorType for NeverConnection {
    type Error = ScriptError;
}

impl Read for NeverConnection {
    async fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }
}

impl Write for NeverConnection {
    async fn write(&mut self, _buffer: &[u8]) -> Result<usize, Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Err(ScriptError(ErrorKind::NotConnected))
    }
}
