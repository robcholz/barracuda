//! Private ABI between the WASI guest and its browser Worker host.

#[link(wasm_import_module = "barracuda_browser")]
unsafe extern "C" {
    fn boot_image_len() -> u32;
    fn boot_image_read(bytes: *mut u8, length: u32) -> i32;
    fn flash_size() -> i32;
    fn flash_truncate(size: u32) -> i32;
    fn flash_read(offset: u32, bytes: *mut u8, length: u32) -> i32;
    fn flash_write(offset: u32, bytes: *const u8, length: u32) -> i32;
    fn flash_flush() -> i32;
    fn flash_close();
    fn monotonic_micros() -> f64;
    fn schedule_alarm(delay_millis: u32) -> i32;
    fn clear_alarm(token: i32);
    fn schedule_executor_poll();
    fn random_fill(bytes: *mut u8, length: u32) -> i32;
    fn http_request_port() -> u32;
    fn http_request_len() -> i32;
    fn http_request_read(bytes: *mut u8, length: u32) -> i32;
    fn http_response_write(bytes: *const u8, length: u32) -> i32;
    fn http_response_finish() -> i32;
    fn socket_open_id() -> u32;
    fn socket_open_port() -> u32;
    fn socket_open_len() -> i32;
    fn socket_open_read(bytes: *mut u8, length: u32) -> i32;
    fn socket_event_kind(id: u32) -> i32;
    fn socket_event_len(id: u32) -> i32;
    fn socket_event_text(id: u32) -> i32;
    fn socket_event_read(id: u32, bytes: *mut u8, length: u32) -> i32;
    fn socket_opened(id: u32, protocol: *const u8, protocol_length: u32) -> i32;
    fn socket_message(id: u32, bytes: *const u8, length: u32, text: i32) -> i32;
    fn socket_error(id: u32, bytes: *const u8, length: u32) -> i32;
    fn socket_closed(id: u32) -> i32;
    fn post_event(kind: u32, bytes: *const u8, length: u32);
}

const EVENT_STARTED: u32 = 1;
const EVENT_ERROR: u32 = 3;

pub(crate) fn system_image() -> Result<Vec<u8>, HostError> {
    let length = unsafe { boot_image_len() };
    let mut image = vec![0; length as usize];
    let actual = unsafe { boot_image_read(image.as_mut_ptr(), length) };
    transferred("read boot image", actual, length)?;
    Ok(image)
}

pub(crate) fn stored_flash_size() -> Result<usize, HostError> {
    let size = unsafe { flash_size() };
    usize::try_from(size).map_err(|_| HostError::new("read flash size", size))
}

pub(crate) fn truncate_flash(size: usize) -> Result<(), HostError> {
    let size = u32::try_from(size).map_err(|_| HostError::new("truncate flash", -1))?;
    completed("truncate flash", unsafe { flash_truncate(size) })
}

pub(crate) fn read_flash(offset: usize, bytes: &mut [u8]) -> Result<(), HostError> {
    let offset = u32::try_from(offset).map_err(|_| HostError::new("read flash", -1))?;
    let length = u32::try_from(bytes.len()).map_err(|_| HostError::new("read flash", -1))?;
    let actual = unsafe { flash_read(offset, bytes.as_mut_ptr(), length) };
    transferred("read flash", actual, length)
}

pub(crate) fn write_flash(offset: usize, bytes: &[u8]) -> Result<(), HostError> {
    let offset = u32::try_from(offset).map_err(|_| HostError::new("write flash", -1))?;
    let length = u32::try_from(bytes.len()).map_err(|_| HostError::new("write flash", -1))?;
    let actual = unsafe { flash_write(offset, bytes.as_ptr(), length) };
    transferred("write flash", actual, length)
}

pub(crate) fn flush_flash() -> Result<(), HostError> {
    completed("flush flash", unsafe { flash_flush() })
}

pub(crate) fn close_flash() {
    unsafe { flash_close() }
}

pub(crate) fn now_micros() -> u64 {
    unsafe { monotonic_micros() }.max(0.0) as u64
}

pub(crate) fn set_alarm(delay_millis: u32) -> Result<i32, HostError> {
    let token = unsafe { schedule_alarm(delay_millis) };
    if token < 0 {
        Err(HostError::new("schedule alarm", token))
    } else {
        Ok(token)
    }
}

pub(crate) fn cancel_alarm(token: i32) {
    unsafe { clear_alarm(token) }
}

pub(crate) fn pend_executor() {
    unsafe { schedule_executor_poll() }
}

pub(crate) fn fill_random(bytes: &mut [u8]) -> Result<(), HostError> {
    let length = u32::try_from(bytes.len()).map_err(|_| HostError::new("fill random", -1))?;
    completed("fill random", unsafe {
        random_fill(bytes.as_mut_ptr(), length)
    })
}

pub(crate) fn take_http_request() -> Result<Option<HttpRequest>, HostError> {
    let length = unsafe { http_request_len() };
    if length == 0 {
        return Ok(None);
    }
    let length =
        u32::try_from(length).map_err(|_| HostError::new("read page request length", length))?;
    let mut request = vec![0; length as usize];
    let port = read_port("read HTTP target port", unsafe { http_request_port() })?;
    let actual = unsafe { http_request_read(request.as_mut_ptr(), length) };
    transferred("read page request", actual, length)?;
    Ok(Some(HttpRequest { port, request }))
}

pub(crate) fn write_http_response(bytes: &[u8]) -> Result<(), HostError> {
    let length =
        u32::try_from(bytes.len()).map_err(|_| HostError::new("write page response", -1))?;
    let actual = unsafe { http_response_write(bytes.as_ptr(), length) };
    transferred("write page response", actual, length)
}

pub(crate) fn finish_http_response() -> Result<(), HostError> {
    completed("finish page response", unsafe { http_response_finish() })
}

pub(crate) fn take_socket_open() -> Result<Option<SocketOpen>, HostError> {
    let length = unsafe { socket_open_len() };
    if length == 0 {
        return Ok(None);
    }
    let length = u32::try_from(length)
        .map_err(|_| HostError::new("read socket handshake length", length))?;
    let id = unsafe { socket_open_id() };
    let port = read_port("read socket target port", unsafe { socket_open_port() })?;
    let mut handshake = vec![0; length as usize];
    let actual = unsafe { socket_open_read(handshake.as_mut_ptr(), length) };
    transferred("read socket handshake", actual, length)?;
    Ok(Some(SocketOpen {
        id,
        port,
        handshake,
    }))
}

pub(crate) fn take_socket_event(id: u32) -> Result<Option<SocketEvent>, HostError> {
    let kind = unsafe { socket_event_kind(id) };
    if kind == 0 {
        return Ok(None);
    }
    let length = unsafe { socket_event_len(id) };
    let length = u32::try_from(length)
        .map_err(|_| HostError::new("read page socket event length", length))?;
    let mut payload = vec![0; length as usize];
    let text = unsafe { socket_event_text(id) } != 0;
    let actual = unsafe { socket_event_read(id, payload.as_mut_ptr(), length) };
    transferred("read page socket event", actual, length)?;
    match kind {
        1 => Ok(Some(SocketEvent::Send { payload, text })),
        2 => Ok(Some(SocketEvent::Close)),
        status => Err(HostError::new("decode page socket event", status)),
    }
}

pub(crate) fn report_socket_opened(id: u32, protocol: &str) -> Result<(), HostError> {
    let length =
        u32::try_from(protocol.len()).map_err(|_| HostError::new("open page socket", -1))?;
    completed("open page socket", unsafe {
        socket_opened(id, protocol.as_ptr(), length)
    })
}

pub(crate) fn report_socket_message(id: u32, bytes: &[u8], text: bool) -> Result<(), HostError> {
    let length =
        u32::try_from(bytes.len()).map_err(|_| HostError::new("write page socket message", -1))?;
    completed("write page socket message", unsafe {
        socket_message(id, bytes.as_ptr(), length, i32::from(text))
    })
}

pub(crate) fn report_socket_error(id: u32, message: &str) -> Result<(), HostError> {
    let length =
        u32::try_from(message.len()).map_err(|_| HostError::new("report page socket error", -1))?;
    completed("report page socket error", unsafe {
        socket_error(id, message.as_ptr(), length)
    })
}

pub(crate) fn report_socket_closed(id: u32) -> Result<(), HostError> {
    completed("close page socket", unsafe { socket_closed(id) })
}

pub(crate) struct HttpRequest {
    pub(crate) port: u16,
    pub(crate) request: Vec<u8>,
}

pub(crate) struct SocketOpen {
    pub(crate) id: u32,
    pub(crate) port: u16,
    pub(crate) handshake: Vec<u8>,
}

pub(crate) enum SocketEvent {
    Send { payload: Vec<u8>, text: bool },
    Close,
}

#[doc(hidden)]
pub fn report_started() {
    report(EVENT_STARTED, &[]);
}

pub(crate) fn report_error(message: &str) {
    report(EVENT_ERROR, message.as_bytes());
}

fn report(kind: u32, bytes: &[u8]) {
    let Ok(length) = u32::try_from(bytes.len()) else {
        return;
    };
    unsafe { post_event(kind, bytes.as_ptr(), length) }
}

fn completed(operation: &'static str, code: i32) -> Result<(), HostError> {
    if code == 0 {
        Ok(())
    } else {
        Err(HostError::new(operation, code))
    }
}

fn transferred(operation: &'static str, actual: i32, expected: u32) -> Result<(), HostError> {
    if u32::try_from(actual) == Ok(expected) {
        Ok(())
    } else {
        Err(HostError::new(operation, actual))
    }
}

fn read_port(operation: &'static str, port: u32) -> Result<u16, HostError> {
    u16::try_from(port)
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| HostError::new(operation, i32::try_from(port).unwrap_or(-1)))
}

#[derive(Debug, thiserror::Error)]
#[error("Browser host failed to {operation} (status {status})")]
pub(crate) struct HostError {
    operation: &'static str,
    status: i32,
}

impl HostError {
    const fn new(operation: &'static str, status: i32) -> Self {
        Self { operation, status }
    }
}
