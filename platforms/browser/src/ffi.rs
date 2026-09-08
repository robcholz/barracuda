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
    fn portal_request_len() -> i32;
    fn portal_request_read(bytes: *mut u8, length: u32) -> i32;
    fn portal_response_write(bytes: *const u8, length: u32) -> i32;
    fn portal_response_finish() -> i32;
    fn socket_event_kind() -> i32;
    fn socket_event_id() -> u32;
    fn socket_event_len() -> i32;
    fn socket_event_read(bytes: *mut u8, length: u32) -> i32;
    fn socket_opened(id: u32) -> i32;
    fn socket_message(id: u32, bytes: *const u8, length: u32, text: i32) -> i32;
    fn socket_closed(id: u32) -> i32;
    fn post_event(kind: u32, bytes: *const u8, length: u32);
}

const EVENT_STARTED: u32 = 1;
const EVENT_DEVICE_URL: u32 = 2;
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

pub(crate) fn take_portal_request() -> Result<Option<Vec<u8>>, HostError> {
    let length = unsafe { portal_request_len() };
    if length == 0 {
        return Ok(None);
    }
    let length =
        u32::try_from(length).map_err(|_| HostError::new("read page request length", length))?;
    let mut request = vec![0; length as usize];
    let actual = unsafe { portal_request_read(request.as_mut_ptr(), length) };
    transferred("read page request", actual, length)?;
    Ok(Some(request))
}

pub(crate) fn write_portal_response(bytes: &[u8]) -> Result<(), HostError> {
    let length =
        u32::try_from(bytes.len()).map_err(|_| HostError::new("write page response", -1))?;
    let actual = unsafe { portal_response_write(bytes.as_ptr(), length) };
    transferred("write page response", actual, length)
}

pub(crate) fn finish_portal_response() -> Result<(), HostError> {
    completed("finish page response", unsafe { portal_response_finish() })
}

pub(crate) fn take_socket_event() -> Result<Option<SocketEvent>, HostError> {
    let kind = unsafe { socket_event_kind() };
    if kind == 0 {
        return Ok(None);
    }
    let id = unsafe { socket_event_id() };
    let length = unsafe { socket_event_len() };
    let length = u32::try_from(length)
        .map_err(|_| HostError::new("read page socket event length", length))?;
    let mut payload = vec![0; length as usize];
    let actual = unsafe { socket_event_read(payload.as_mut_ptr(), length) };
    transferred("read page socket event", actual, length)?;
    match kind {
        1 => Ok(Some(SocketEvent::Open { id })),
        2 => Ok(Some(SocketEvent::Send { id, payload })),
        3 => Ok(Some(SocketEvent::Close { id })),
        status => Err(HostError::new("decode page socket event", status)),
    }
}

pub(crate) fn report_socket_opened(id: u32) -> Result<(), HostError> {
    completed("open page socket", unsafe { socket_opened(id) })
}

pub(crate) fn report_socket_message(id: u32, bytes: &[u8], text: bool) -> Result<(), HostError> {
    let length =
        u32::try_from(bytes.len()).map_err(|_| HostError::new("write page socket message", -1))?;
    completed("write page socket message", unsafe {
        socket_message(id, bytes.as_ptr(), length, i32::from(text))
    })
}

pub(crate) fn report_socket_closed(id: u32) -> Result<(), HostError> {
    completed("close page socket", unsafe { socket_closed(id) })
}

pub(crate) enum SocketEvent {
    Open { id: u32 },
    Send { id: u32, payload: Vec<u8> },
    Close { id: u32 },
}

#[doc(hidden)]
pub fn report_started() {
    report(EVENT_STARTED, &[]);
}

pub(crate) fn report_device_url(url: &str) {
    report(EVENT_DEVICE_URL, url.as_bytes());
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
