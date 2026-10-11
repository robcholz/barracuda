//! Scripted HTTP server on an in-process Embassy Net stack.

use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use embassy_futures::join::join_array;
use embassy_net::tcp::TcpSocket;
use embassy_net::Stack;
use embedded_io_async::Write as _;

use crate::ScriptStep;

const SOCKET_BUFFER_BYTES: usize = 8 * 1024;

#[derive(Default)]
struct State {
    steps: VecDeque<ScriptStep>,
    requests: Vec<Vec<u8>>,
    connect_count: usize,
}

/// An HTTP server on a [`crate::LoopbackNetwork`] stack that answers each
/// request with the next [`ScriptStep`], in arrival order across connections.
///
/// [`ScriptStep::ConnectError`] resets the next accepted connection before it
/// sends anything. A response built with
/// [`ScriptStep::pending_after_headers`] holds its connection open until the
/// client closes it. Other responses keep the connection for the next request.
#[derive(Clone, Default)]
pub struct ScriptedServer {
    state: Rc<RefCell<State>>,
}

impl ScriptedServer {
    /// Creates a server answering with `steps` in order.
    #[must_use]
    pub fn new(steps: impl IntoIterator<Item = ScriptStep>) -> Self {
        Self {
            state: Rc::new(RefCell::new(State {
                steps: steps.into_iter().collect(),
                ..State::default()
            })),
        }
    }

    /// Appends one more outcome.
    pub fn push(&self, step: ScriptStep) {
        self.state.borrow_mut().steps.push_back(step);
    }

    /// Returns every request received, head and body, in arrival order.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.state
            .borrow()
            .requests
            .iter()
            .map(|request| String::from_utf8_lossy(request).to_string())
            .collect()
    }

    /// Returns the number of unconsumed outcomes.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.state.borrow().steps.len()
    }

    /// Returns the number of accepted connections.
    #[must_use]
    pub fn connect_count(&self) -> usize {
        self.state.borrow().connect_count
    }

    /// Serves `port` on `stack` with `N` concurrent connections, forever.
    pub async fn run<const N: usize>(&self, stack: Stack<'static>, port: u16) {
        join_array(core::array::from_fn::<_, N, _>(|_| {
            self.listen(stack, port)
        }))
        .await;
    }

    async fn listen(&self, stack: Stack<'static>, port: u16) {
        let mut rx = vec![0; SOCKET_BUFFER_BYTES];
        let mut tx = vec![0; SOCKET_BUFFER_BYTES];
        loop {
            let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
            if socket.accept(port).await.is_err() {
                continue;
            }
            let refuse = {
                let mut state = self.state.borrow_mut();
                state.connect_count += 1;
                if matches!(state.steps.front(), Some(ScriptStep::ConnectError(_))) {
                    state.steps.pop_front();
                    true
                } else {
                    false
                }
            };
            if !refuse {
                self.serve(&mut socket).await;
                socket.close();
                let _flushed = socket.flush().await;
            }
            socket.abort();
            let _flushed = socket.flush().await;
        }
    }

    async fn serve(&self, socket: &mut TcpSocket<'_>) {
        let mut pending = Vec::new();
        loop {
            let Some(request) = read_request(socket, &mut pending).await else {
                return;
            };
            self.state.borrow_mut().requests.push(request);
            let step = self.state.borrow_mut().steps.pop_front();
            let Some(ScriptStep::Response {
                bytes,
                max_read,
                pending_after,
            }) = step
            else {
                return;
            };
            for chunk in bytes.chunks(max_read) {
                if socket.write_all(chunk).await.is_err() || socket.flush().await.is_err() {
                    return;
                }
            }
            if pending_after {
                let mut sink = [0; 256];
                while matches!(socket.read(&mut sink).await, Ok(count) if count > 0) {}
                return;
            }
        }
    }
}

/// Reads one request: its head, then a `Content-Length` or chunked body.
/// Returns `None` when the client closes the connection first.
async fn read_request(socket: &mut TcpSocket<'_>, pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    let head_end = loop {
        if let Some(index) = find(pending, b"\r\n\r\n") {
            break index + 4;
        }
        fill(socket, pending).await?;
    };
    let head = String::from_utf8_lossy(&pending[..head_end]).to_ascii_lowercase();
    let content_length = head.lines().find_map(|line| {
        line.strip_prefix("content-length:")
            .and_then(|value| value.trim().parse::<usize>().ok())
    });
    let chunked = head
        .lines()
        .any(|line| line.starts_with("transfer-encoding:") && line.contains("chunked"));
    let end = if let Some(length) = content_length {
        while pending.len() < head_end + length {
            fill(socket, pending).await?;
        }
        head_end + length
    } else if chunked {
        loop {
            if let Some(end) = chunked_end(pending, head_end) {
                break end;
            }
            fill(socket, pending).await?;
        }
    } else {
        head_end
    };
    let request = pending[..end].to_vec();
    pending.drain(..end);
    Some(request)
}

async fn fill(socket: &mut TcpSocket<'_>, pending: &mut Vec<u8>) -> Option<()> {
    let mut buffer = [0; 1024];
    match socket.read(&mut buffer).await {
        Ok(count) if count > 0 => {
            pending.extend_from_slice(&buffer[..count]);
            Some(())
        }
        _ => None,
    }
}

/// Returns the end of a complete chunked body starting at `start`.
fn chunked_end(buffer: &[u8], start: usize) -> Option<usize> {
    let mut offset = start;
    loop {
        let line = find(buffer.get(offset..)?, b"\r\n")?;
        let size = core::str::from_utf8(&buffer[offset..offset + line]).ok()?;
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
        offset += line + 2;
        if size == 0 {
            return (buffer.get(offset..offset + 2)? == b"\r\n").then_some(offset + 2);
        }
        offset += size + 2;
        if buffer.len() < offset {
            return None;
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
