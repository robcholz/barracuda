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

/// One queue of outcomes and the requests it answered.
#[derive(Default)]
struct Queue {
    /// Text a request line contains to take this queue; `None` for the
    /// default queue.
    pattern: Option<String>,
    steps: VecDeque<ScriptStep>,
    requests: Vec<Vec<u8>>,
}

#[derive(Default)]
struct State {
    /// The default queue first, then routes in the order they were added.
    queues: Vec<Queue>,
    connect_count: usize,
}

impl State {
    fn queue_for(&self, request: &[u8]) -> usize {
        let line = request
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        let line = String::from_utf8_lossy(line);
        self.queues
            .iter()
            .position(|queue| {
                queue
                    .pattern
                    .as_deref()
                    .is_some_and(|pattern| line.contains(pattern))
            })
            .unwrap_or(0)
    }
}

/// An HTTP server on a [`crate::LoopbackNetwork`] stack that answers each
/// request with the next [`ScriptStep`], in arrival order across connections.
///
/// [`ScriptStep::ConnectError`] at the front of the default queue resets the
/// next accepted connection before it sends anything. A response built with
/// [`ScriptStep::pending_after_headers`] holds its connection open until the
/// client closes it. Other responses keep the connection for the next request.
///
/// A [`ScriptedRoute`] answers the requests whose request line contains its
/// pattern from a queue of its own, so concurrent clients of one server (a
/// long poll and ordinary sends) each get their own outcomes. A
/// `ConnectError` there resets the connection the matching request came on.
#[derive(Clone)]
pub struct ScriptedServer {
    state: Rc<RefCell<State>>,
}

/// The outcomes and requests of one [`ScriptedServer::route`].
#[derive(Clone)]
pub struct ScriptedRoute {
    state: Rc<RefCell<State>>,
    index: usize,
}

impl ScriptedServer {
    /// Creates a server answering with `steps` in order.
    #[must_use]
    pub fn new(steps: impl IntoIterator<Item = ScriptStep>) -> Self {
        Self {
            state: Rc::new(RefCell::new(State {
                queues: vec![Queue {
                    steps: steps.into_iter().collect(),
                    ..Queue::default()
                }],
                connect_count: 0,
            })),
        }
    }

    /// Answers requests whose request line contains `pattern` with `steps`
    /// instead of the default queue.
    #[must_use]
    pub fn route(
        &self,
        pattern: &str,
        steps: impl IntoIterator<Item = ScriptStep>,
    ) -> ScriptedRoute {
        let mut state = self.state.borrow_mut();
        state.queues.push(Queue {
            pattern: Some(pattern.to_string()),
            steps: steps.into_iter().collect(),
            requests: Vec::new(),
        });
        ScriptedRoute {
            state: Rc::clone(&self.state),
            index: state.queues.len() - 1,
        }
    }

    /// Appends one more outcome to the default queue.
    pub fn push(&self, step: ScriptStep) {
        push(&self.state, 0, step);
    }

    /// Returns the requests the default queue answered, head and body, in
    /// arrival order.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        requests(&self.state, 0)
    }

    /// Returns the number of unconsumed outcomes of the default queue.
    #[must_use]
    pub fn remaining(&self) -> usize {
        remaining(&self.state, 0)
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
                let default = &mut state.queues[0].steps;
                if matches!(default.front(), Some(ScriptStep::ConnectError(_))) {
                    default.pop_front();
                    true
                } else {
                    false
                }
            };
            if !refuse {
                self.serve(&mut socket).await;
            }
            // Every response is flushed and every client has closed or gone
            // by now, so the connection ends at once and the listener
            // accepts the next one.
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
            let step = {
                let mut state = self.state.borrow_mut();
                let index = state.queue_for(&request);
                let queue = &mut state.queues[index];
                queue.requests.push(request);
                queue.steps.pop_front()
            };
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

impl ScriptedRoute {
    /// Appends one more outcome.
    pub fn push(&self, step: ScriptStep) {
        push(&self.state, self.index, step);
    }

    /// Returns the requests this route answered, head and body, in arrival
    /// order.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        requests(&self.state, self.index)
    }

    /// Returns the number of unconsumed outcomes.
    #[must_use]
    pub fn remaining(&self) -> usize {
        remaining(&self.state, self.index)
    }
}

fn push(state: &RefCell<State>, index: usize, step: ScriptStep) {
    state.borrow_mut().queues[index].steps.push_back(step);
}

fn requests(state: &RefCell<State>, index: usize) -> Vec<String> {
    state.borrow().queues[index]
        .requests
        .iter()
        .map(|request| String::from_utf8_lossy(request).to_string())
        .collect()
}

fn remaining(state: &RefCell<State>, index: usize) -> usize {
    state.borrow().queues[index].steps.len()
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
