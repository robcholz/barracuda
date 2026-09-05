use alloc::rc::Rc;
use core::fmt::Write as _;

use barracuda_event_router::{
    JsonHandler, JsonPayload, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RpcError, json_schema,
};
use time::OffsetDateTime;

use crate::{ClockError, UnixMillis, UtcClock};

/// Reads the network-synchronized UTC clock.
pub struct Now;

impl JsonRpcSchema for Now {
    const ADDRESS: &'static str = "time.now";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("now", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("now", response);
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 34;
}

/// Builds the reusable JSON handler for [`Now`].
pub fn now_handler(clock: Rc<UtcClock>) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let clock = Rc::clone(&clock);
        async move {
            if request.as_str()? != "{}" {
                return Err(RpcError::InvalidJson);
            }
            match clock.now().and_then(Rfc3339Utc::try_from) {
                Ok(utc) => response.write(&utc).await,
                Err(error) => response.write(&ErrorResponse(error)).await,
            }
        }
    }
}

struct Rfc3339Utc {
    calendar: OffsetDateTime,
    millisecond: u16,
}

impl TryFrom<UnixMillis> for Rfc3339Utc {
    type Error = ClockError;

    fn try_from(timestamp: UnixMillis) -> Result<Self, Self::Error> {
        let unix_millis = u64::from(timestamp);
        let unix_seconds =
            i64::try_from(unix_millis / 1_000).map_err(|_error| ClockError::OutOfRange)?;
        let calendar = OffsetDateTime::from_unix_timestamp(unix_seconds)
            .map_err(|_error| ClockError::OutOfRange)?;
        if !(0..=9_999).contains(&calendar.year()) {
            return Err(ClockError::OutOfRange);
        }
        let millisecond =
            u16::try_from(unix_millis % 1_000).map_err(|_error| ClockError::OutOfRange)?;
        Ok(Self {
            calendar,
            millisecond,
        })
    }
}

impl JsonPayload for Rfc3339Utc {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        Ok(34)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        let capacity = destination.len();
        let mut writer = SliceWriter::new(destination);
        write!(
            writer,
            "{{\"utc\":\"{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z\"}}",
            self.calendar.year(),
            u8::from(self.calendar.month()),
            self.calendar.day(),
            self.calendar.hour(),
            self.calendar.minute(),
            self.calendar.second(),
            self.millisecond,
        )
        .map_err(|_error| RpcError::FrameTooLarge { size: 34, capacity })?;
        Ok(writer.written)
    }
}

struct ErrorResponse(ClockError);

impl JsonPayload for ErrorResponse {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        12_usize
            .checked_add(self.0.code().len())
            .ok_or(RpcError::InvalidFrameState)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        let size = self.encoded_len()?;
        let capacity = destination.len();
        let mut writer = SliceWriter::new(destination);
        write!(writer, "{{\"error\":\"{}\"}}", self.0.code())
            .map_err(|_error| RpcError::FrameTooLarge { size, capacity })?;
        Ok(writer.written)
    }
}

struct SliceWriter<'a> {
    destination: &'a mut [u8],
    written: usize,
}

impl<'a> SliceWriter<'a> {
    const fn new(destination: &'a mut [u8]) -> Self {
        Self {
            destination,
            written: 0,
        }
    }
}

impl core::fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        let end = self
            .written
            .checked_add(value.len())
            .ok_or(core::fmt::Error)?;
        let output = self
            .destination
            .get_mut(self.written..end)
            .ok_or(core::fmt::Error)?;
        output.copy_from_slice(value.as_bytes());
        self.written = end;
        Ok(())
    }
}
