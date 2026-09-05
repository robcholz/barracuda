use core::fmt::Write as _;

use barracuda_event_router::{JsonPayload, RpcError};

use crate::{
    model::{DueOccurrence, ScheduleId},
    schedule::ScheduleError,
};

pub(crate) struct ScheduleAccepted {
    id: ScheduleId,
}

impl ScheduleAccepted {
    pub(crate) const fn new(id: ScheduleId) -> Self {
        Self { id }
    }
}

impl JsonPayload for ScheduleAccepted {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        checked_add(9, self.id.as_str().len())
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_document(self.encoded_len()?, destination, |writer| {
            write!(writer, "{{\"id\":\"{}\"}}", self.id.as_str())
        })
    }
}

pub(crate) struct ScheduleCancelled {
    id: ScheduleId,
    completed_runs: u64,
}

impl ScheduleCancelled {
    pub(crate) const fn new(id: ScheduleId, completed_runs: u64) -> Self {
        Self { id, completed_runs }
    }
}

impl JsonPayload for ScheduleCancelled {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        checked_add(
            checked_add(27, self.id.as_str().len())?,
            decimal_len(self.completed_runs),
        )
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_document(self.encoded_len()?, destination, |writer| {
            write!(
                writer,
                "{{\"id\":\"{}\",\"completed_runs\":{}}}",
                self.id.as_str(),
                self.completed_runs,
            )
        })
    }
}

pub(crate) struct ScheduleRejected {
    error: ScheduleError,
}

impl ScheduleRejected {
    pub(crate) const fn new(error: ScheduleError) -> Self {
        Self { error }
    }
}

impl JsonPayload for ScheduleRejected {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        checked_add(12, self.error.code().len())
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_document(self.encoded_len()?, destination, |writer| {
            write!(writer, "{{\"error\":\"{}\"}}", self.error.code())
        })
    }
}

pub(crate) struct Triggered {
    id: ScheduleId,
    run_number: u64,
}

impl Triggered {
    pub(crate) const fn new(occurrence: DueOccurrence) -> Self {
        Self {
            id: occurrence.id(),
            run_number: occurrence.run_number(),
        }
    }
}

impl JsonPayload for Triggered {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        checked_add(
            checked_add(23, self.id.as_str().len())?,
            decimal_len(self.run_number),
        )
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_document(self.encoded_len()?, destination, |writer| {
            write!(
                writer,
                "{{\"id\":\"{}\",\"run_number\":{}}}",
                self.id.as_str(),
                self.run_number,
            )
        })
    }
}

fn checked_add(left: usize, right: usize) -> Result<usize, RpcError> {
    left.checked_add(right).ok_or(RpcError::InvalidFrameState)
}

fn decimal_len(value: u64) -> usize {
    let mut remaining = value;
    let mut digits = 1_usize;
    while remaining >= 10 {
        remaining /= 10;
        digits = digits.saturating_add(1);
    }
    digits
}

fn write_document(
    size: usize,
    destination: &mut [u8],
    write: impl FnOnce(&mut SliceWriter<'_>) -> core::fmt::Result,
) -> Result<usize, RpcError> {
    let capacity = destination.len();
    if size > capacity {
        return Err(RpcError::FrameTooLarge { size, capacity });
    }
    let mut writer = SliceWriter::new(destination);
    write(&mut writer).map_err(|_error| RpcError::FrameTooLarge { size, capacity })?;
    if writer.written != size {
        return Err(RpcError::InvalidFrameState);
    }
    Ok(size)
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use barracuda_event_router::JsonPayload;

    use super::{ScheduleCancelled, ScheduleRejected, Triggered};
    use crate::{
        model::{DueOccurrence, ScheduleId},
        schedule::ScheduleError,
    };

    #[test]
    fn bounded_payloads_write_exact_documents() {
        let id = ScheduleId::new("abcdefghijklmnop").expect("maximum-length ID");
        let cancelled = ScheduleCancelled::new(id, u64::MAX);
        assert_eq!(cancelled.encoded_len().expect("measure cancellation"), 63);
        let mut cancellation = [0_u8; 64];
        let written = cancelled
            .write_json(&mut cancellation)
            .expect("write cancellation");
        assert_eq!(
            core::str::from_utf8(cancellation.get(..written).expect("written range"))
                .expect("cancellation UTF-8"),
            r#"{"id":"abcdefghijklmnop","completed_runs":18446744073709551615}"#
        );

        let triggered = Triggered::new(DueOccurrence::new(id, 0, u64::MAX));
        assert_eq!(triggered.encoded_len().expect("measure Event"), 59);

        let rejected = ScheduleRejected::new(ScheduleError::StorageUnavailable);
        assert_eq!(rejected.encoded_len().expect("measure rejection"), 31);
    }
}
