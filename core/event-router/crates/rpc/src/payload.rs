//! Asynchronous wire-level RPC payload IO over fixed lane frames.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::marker::PhantomData;
use core::task::{Context, Poll, Waker};

use getset::{Getters, MutGetters};

use super::lane::{BorrowedFrame, LaneFrameKind, LaneIo, LaneReader, LaneWriter, ReservedFrame};
use super::registry::{PreparedCalls, RpcFuture};
use super::{RpcAddress, RpcError, RpcResult};

/// One zero-copy response or method-error frame borrowed from an RPC lane.
///
/// The lane cannot publish its next response frame or be reused by another
/// call until this value is dropped.
pub struct RpcPayloadFrame {
    frame: BorrowedFrame,
}

impl RpcPayloadFrame {
    pub(crate) fn from_frame(frame: BorrowedFrame) -> Self {
        Self { frame }
    }
}

impl AsRef<[u8]> for RpcPayloadFrame {
    fn as_ref(&self) -> &[u8] {
        self.frame.as_bytes()
    }
}

impl core::fmt::Debug for RpcPayloadFrame {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RpcPayloadFrame")
            .field("len", &self.as_ref().len())
            .finish_non_exhaustive()
    }
}

/// A reserved request frame that may be filled directly in lane storage.
///
/// Dropping this value without calling [`commit`](Self::commit) cancels the
/// reservation and publishes nothing.
#[must_use = "a reserved payload frame must be committed to publish it"]
pub struct RpcPayloadWriteFrame<'a> {
    frame: ReservedFrame,
    writer: PhantomData<&'a mut RpcPayloadWriter>,
}

impl RpcPayloadWriteFrame<'_> {
    /// Publishes the initialized prefix of this frame.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::FrameTooLarge`] when `written` exceeds the writable
    /// slice exposed by [`AsMut<[u8]>`](AsMut).
    pub fn commit(self, written: usize) -> RpcResult<()> {
        self.frame.commit(written, LaneFrameKind::Message)
    }
}

impl AsMut<[u8]> for RpcPayloadWriteFrame<'_> {
    fn as_mut(&mut self) -> &mut [u8] {
        self.frame.as_mut()
    }
}

impl core::fmt::Debug for RpcPayloadWriteFrame<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RpcPayloadWriteFrame")
            .field("capacity", &self.frame.as_ref().len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
enum PayloadSide {
    Writer,
    Reader(usize),
}

enum PayloadCallPhase {
    Acquiring(PreparedCalls),
    Active,
    Complete,
    Failed(RpcError),
}

struct PayloadBranchState {
    handler: Option<RpcFuture<'static>>,
    response: Option<LaneReader>,
    alive: bool,
    complete: bool,
    error: Option<RpcError>,
    waker: Option<Waker>,
}

struct PayloadCallState {
    phase: PayloadCallPhase,
    request: Option<LaneWriter>,
    writer_alive: bool,
    writer_waker: Option<Waker>,
    branches: Vec<PayloadBranchState>,
}

impl PayloadCallState {
    fn update_waker(&mut self, side: PayloadSide, waker: &Waker) {
        let slot = match side {
            PayloadSide::Writer => &mut self.writer_waker,
            PayloadSide::Reader(index) => match self.branches.get_mut(index) {
                Some(branch) => &mut branch.waker,
                None => return,
            },
        };
        if slot
            .as_ref()
            .is_none_or(|registered| !registered.will_wake(waker))
        {
            *slot = Some(waker.clone());
        }
    }

    fn take_peer_wakers(&mut self, side: PayloadSide) -> Vec<Waker> {
        match side {
            PayloadSide::Writer => self
                .branches
                .iter_mut()
                .filter_map(|branch| branch.waker.take())
                .collect(),
            PayloadSide::Reader(_) => self.writer_waker.take().into_iter().collect(),
        }
    }
}

struct PayloadCallShared {
    state: RefCell<PayloadCallState>,
}

struct PayloadInputDriver {
    input: Option<RpcFuture<'static>>,
    error: Option<RpcError>,
}

impl PayloadInputDriver {
    fn poll(&mut self, context: &mut Context<'_>) -> Poll<RpcResult<()>> {
        if let Some(error) = &self.error {
            return Poll::Ready(Err(error.clone()));
        }
        let Some(input) = self.input.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        match input.as_mut().poll(context) {
            Poll::Ready(Ok(())) => {
                self.input = None;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                self.input = None;
                self.error = Some(error.clone());
                Poll::Ready(Err(error))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl PayloadCallShared {
    fn new(prepared: PreparedCalls, branch_count: usize) -> Self {
        Self {
            state: RefCell::new(PayloadCallState {
                phase: PayloadCallPhase::Acquiring(prepared),
                request: None,
                writer_alive: true,
                writer_waker: None,
                branches: (0..branch_count)
                    .map(|_| PayloadBranchState {
                        handler: None,
                        response: None,
                        alive: true,
                        complete: false,
                        error: None,
                        waker: None,
                    })
                    .collect(),
            }),
        }
    }

    fn poll_ready(&self, side: PayloadSide, context: &mut Context<'_>) -> Poll<RpcResult<()>> {
        let mut completed_handlers = Vec::new();
        let mut handler_wakers = Vec::new();
        let (result, mut peer_wakers) = {
            let mut state = self.state.borrow_mut();
            state.update_waker(side, context.waker());
            if let PayloadCallPhase::Failed(error) = &state.phase {
                return Poll::Ready(Err(error.clone()));
            }

            let acquire_poll = match &mut state.phase {
                PayloadCallPhase::Acquiring(prepared) => Some(prepared.poll_acquire(context)),
                PayloadCallPhase::Active
                | PayloadCallPhase::Complete
                | PayloadCallPhase::Failed(_) => None,
            };
            match acquire_poll {
                Some(Poll::Pending) => return Poll::Pending,
                Some(Poll::Ready(Err(error))) => {
                    state.phase = PayloadCallPhase::Failed(error.clone());
                    let peer = state.take_peer_wakers(side);
                    (Poll::Ready(Err(error)), peer)
                }
                Some(Poll::Ready(Ok(mut lanes))) => {
                    let PayloadCallPhase::Acquiring(prepared) =
                        core::mem::replace(&mut state.phase, PayloadCallPhase::Complete)
                    else {
                        return Poll::Ready(Err(RpcError::InvalidLaneState));
                    };
                    let targets = prepared.into_targets();
                    if targets.len() != lanes.len() || targets.len() != state.branches.len() {
                        return Poll::Ready(Err(RpcError::InvalidLaneState));
                    }
                    let Some(source) = lanes.first_mut() else {
                        return Poll::Ready(Err(RpcError::InvalidLaneState));
                    };
                    let mut request_readers = match source.share_request_readers(targets.len()) {
                        Ok(readers) => readers.into_iter(),
                        Err(error) => return Poll::Ready(Err(error)),
                    };
                    let Some(request_writer) = source.request_writer.take() else {
                        return Poll::Ready(Err(RpcError::InvalidLaneState));
                    };
                    for ((target, lane), branch) in targets
                        .into_iter()
                        .zip(lanes)
                        .zip(state.branches.iter_mut())
                    {
                        let Some(request_reader) = request_readers.next() else {
                            return Poll::Ready(Err(RpcError::InvalidLaneState));
                        };
                        let LaneIo {
                            request_reader: unused_request_reader,
                            request_writer: unused_request_writer,
                            response_reader,
                            response_writer,
                        } = lane;
                        drop(unused_request_reader);
                        drop(unused_request_writer);
                        branch.handler = Some(target.start(request_reader, response_writer));
                        branch.response = branch.alive.then_some(response_reader);
                    }
                    state.request = state.writer_alive.then_some(request_writer);
                    state.phase = PayloadCallPhase::Active;
                    let peer = state.take_peer_wakers(side);
                    let mut result = Self::poll_handlers(
                        &mut state,
                        context,
                        &mut completed_handlers,
                        &mut handler_wakers,
                    );
                    Self::apply_unicast_writer_error(&state, side, &mut result);
                    (result, peer)
                }
                None => {
                    let mut result = Self::poll_handlers(
                        &mut state,
                        context,
                        &mut completed_handlers,
                        &mut handler_wakers,
                    );
                    Self::apply_unicast_writer_error(&state, side, &mut result);
                    let peer = state.take_peer_wakers(side);
                    (result, peer)
                }
            }
        };
        drop(completed_handlers);
        peer_wakers.append(&mut handler_wakers);
        for waker in peer_wakers {
            waker.wake();
        }
        result
    }

    fn poll_handlers(
        state: &mut PayloadCallState,
        context: &mut Context<'_>,
        completed: &mut Vec<RpcFuture<'static>>,
        wakers: &mut Vec<Waker>,
    ) -> Poll<RpcResult<()>> {
        match &state.phase {
            PayloadCallPhase::Failed(error) => {
                return Poll::Ready(Err(error.clone()));
            }
            PayloadCallPhase::Acquiring(_) => return Poll::Pending,
            PayloadCallPhase::Complete => return Poll::Ready(Ok(())),
            PayloadCallPhase::Active => {}
        }
        for branch in &mut state.branches {
            let Some(handler) = branch.handler.as_mut() else {
                continue;
            };
            match handler.as_mut().poll(context) {
                Poll::Ready(Ok(())) => branch.complete = true,
                Poll::Ready(Err(error)) => {
                    branch.complete = true;
                    branch.error = Some(error);
                }
                Poll::Pending => continue,
            }
            if let Some(handler) = branch.handler.take() {
                completed.push(handler);
            }
            if let Some(waker) = branch.waker.take() {
                wakers.push(waker);
            }
        }
        if state.branches.iter().all(|branch| branch.complete) {
            state.phase = PayloadCallPhase::Complete;
        }
        Poll::Ready(Ok(()))
    }

    fn apply_unicast_writer_error(
        state: &PayloadCallState,
        side: PayloadSide,
        result: &mut Poll<RpcResult<()>>,
    ) {
        if !matches!(side, PayloadSide::Writer) || state.branches.len() != 1 {
            return;
        }
        if let Some(error) = state
            .branches
            .first()
            .and_then(|branch| branch.error.clone())
        {
            *result = Poll::Ready(Err(error));
        }
    }

    fn take_request(&self) -> RpcResult<LaneWriter> {
        self.state
            .borrow_mut()
            .request
            .take()
            .ok_or(RpcError::InvalidLaneState)
    }

    fn take_response(&self, index: usize) -> RpcResult<LaneReader> {
        self.state
            .borrow_mut()
            .branches
            .get_mut(index)
            .and_then(|branch| branch.response.take())
            .ok_or(RpcError::InvalidLaneState)
    }

    fn branch_error(&self, index: usize) -> Option<RpcError> {
        self.state
            .borrow()
            .branches
            .get(index)
            .and_then(|branch| branch.error.clone())
    }

    fn drop_side(&self, side: PayloadSide) {
        let mut request = None;
        let mut response = None;
        let peer_wakers: Vec<Waker> = {
            let mut state = self.state.borrow_mut();
            match side {
                PayloadSide::Writer => {
                    state.writer_alive = false;
                    request = state.request.take();
                    state
                        .branches
                        .iter_mut()
                        .filter_map(|branch| branch.waker.take())
                        .collect()
                }
                PayloadSide::Reader(index) => {
                    if let Some(branch) = state.branches.get_mut(index) {
                        branch.alive = false;
                        response = branch.response.take();
                    }
                    state.writer_waker.take().into_iter().collect()
                }
            }
        };
        drop(request);
        drop(response);
        for waker in peer_wakers {
            waker.wake();
        }
    }
}

/// Asynchronous request-side payload writer for one runtime-addressed RPC.
///
/// [`write`](Self::write) publishes at most one request frame and returns the
/// number of bytes accepted. [`write_all`](Self::write_all) repeats that
/// operation across frames. The target Method's fixed request wire size is the
/// per-frame capacity; callers do not provide a separate size limit.
pub struct RpcPayloadWriter {
    shared: Rc<PayloadCallShared>,
    writer: Option<LaneWriter>,
    frame_capacity: usize,
    closed: bool,
}

impl RpcPayloadWriter {
    fn poll_operational(&mut self, context: &mut Context<'_>) -> Poll<RpcResult<()>> {
        if self.closed {
            return Poll::Ready(Err(RpcError::FrameWriterClosed));
        }
        match self.shared.poll_ready(PayloadSide::Writer, context) {
            Poll::Ready(Ok(())) => {
                if self.writer.is_none() {
                    self.writer = Some(self.shared.take_request()?);
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    /// Waits for writable lane storage and reserves one request frame in place.
    ///
    /// The returned slice has exactly the target Method's fixed request wire
    /// size. Dropping the reservation publishes nothing.
    pub async fn reserve(&mut self) -> RpcResult<RpcPayloadWriteFrame<'_>> {
        let frame_capacity = self.frame_capacity;
        let frame = core::future::poll_fn(|context| {
            match self.poll_operational(context) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => return Poll::Pending,
            }
            self.writer
                .as_mut()
                .ok_or(RpcError::InvalidLaneState)?
                .poll_reserve_frame(context)
        })
        .await?
        .limit(frame_capacity)?;
        Ok(RpcPayloadWriteFrame {
            frame,
            writer: PhantomData,
        })
    }

    /// Writes at most one request frame and returns the number of bytes written.
    ///
    /// A slice larger than the Method's request frame is partially written;
    /// pass the remaining suffix to another call or use [`write_all`](Self::write_all).
    pub async fn write(&mut self, bytes: &[u8]) -> RpcResult<usize> {
        if bytes.is_empty() || self.frame_capacity == 0 {
            return Ok(0);
        }
        let mut frame = self.reserve().await?;
        let written = bytes.len().min(frame.as_mut().len());
        let source = bytes.get(..written).ok_or(RpcError::InvalidFrameState)?;
        let destination = frame
            .as_mut()
            .get_mut(..written)
            .ok_or(RpcError::InvalidFrameState)?;
        destination.copy_from_slice(source);
        frame.commit(written)?;
        Ok(written)
    }

    pub(crate) async fn write_frame(&mut self, bytes: &[u8]) -> RpcResult<()> {
        let mut frame = self.reserve().await?;
        if frame.as_mut().len() != bytes.len() {
            return Err(RpcError::InvalidFrameState);
        }
        frame.as_mut().copy_from_slice(bytes);
        frame.commit(bytes.len())
    }

    /// Writes the complete slice, splitting it across request frames as needed.
    pub async fn write_all(&mut self, mut bytes: &[u8]) -> RpcResult<()> {
        while !bytes.is_empty() {
            let written = self.write(bytes).await?;
            if written == 0 {
                return Err(RpcError::PayloadWriteZero);
            }
            bytes = bytes.get(written..).ok_or(RpcError::InvalidFrameState)?;
        }
        Ok(())
    }

    /// Closes the request direction and publishes EOF to the handler.
    pub async fn close(&mut self) -> RpcResult<()> {
        if self.closed {
            return Ok(());
        }
        core::future::poll_fn(|context| self.poll_operational(context)).await?;
        drop(self.writer.take());
        self.shared.drop_side(PayloadSide::Writer);
        self.closed = true;
        Ok(())
    }
}

impl core::fmt::Debug for RpcPayloadWriter {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RpcPayloadWriter")
            .field("frame_capacity", &self.frame_capacity)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl Drop for RpcPayloadWriter {
    fn drop(&mut self) {
        drop(self.writer.take());
        self.shared.drop_side(PayloadSide::Writer);
        self.closed = true;
    }
}

/// Asynchronous response-side payload reader for one runtime-addressed RPC.
pub struct RpcPayloadReader {
    shared: Rc<PayloadCallShared>,
    input_driver: Option<Rc<RefCell<PayloadInputDriver>>>,
    branch_index: usize,
    reader: Option<LaneReader>,
    finished: bool,
}

impl RpcPayloadReader {
    fn poll_operational(&mut self, context: &mut Context<'_>) -> Poll<RpcResult<()>> {
        if self.finished {
            return Poll::Ready(Ok(()));
        }
        if let Some(driver) = &self.input_driver {
            if let Poll::Ready(Err(error)) = driver.borrow_mut().poll(context) {
                return Poll::Ready(Err(error));
            }
        }
        match self
            .shared
            .poll_ready(PayloadSide::Reader(self.branch_index), context)
        {
            Poll::Ready(Ok(())) => {
                if let Some(error) = self.shared.branch_error(self.branch_index) {
                    return Poll::Ready(Err(error));
                }
                if self.reader.is_none() {
                    self.reader = Some(self.shared.take_response(self.branch_index)?);
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    /// Waits for the next response frame.
    ///
    /// `Ok(None)` is response EOF. The inner `Result` distinguishes a normal
    /// response frame from the Method's terminal error frame.
    pub async fn read(&mut self) -> RpcResult<Option<Result<RpcPayloadFrame, RpcPayloadFrame>>> {
        core::future::poll_fn(|context| self.poll_read(context)).await
    }

    pub(crate) fn poll_read(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<RpcResult<Option<Result<RpcPayloadFrame, RpcPayloadFrame>>>> {
        if self.finished {
            return Poll::Ready(Ok(None));
        }
        match self.poll_operational(context) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(error)) => {
                self.finished = true;
                return Poll::Ready(Err(error));
            }
            Poll::Pending => return Poll::Pending,
        }
        let reader = match self.reader.as_mut() {
            Some(reader) => reader,
            None => return Poll::Ready(Err(RpcError::InvalidLaneState)),
        };
        match reader.poll_borrow_frame(context) {
            Poll::Ready(Ok(Some(frame))) => match frame.kind() {
                LaneFrameKind::Message => {
                    Poll::Ready(Ok(Some(Ok(RpcPayloadFrame::from_frame(frame)))))
                }
                LaneFrameKind::MethodError => {
                    self.finished = true;
                    drop(self.reader.take());
                    Poll::Ready(Ok(Some(Err(RpcPayloadFrame::from_frame(frame)))))
                }
            },
            Poll::Ready(Ok(None)) => {
                self.finished = true;
                drop(self.reader.take());
                Poll::Ready(Ok(None))
            }
            Poll::Ready(Err(error)) => {
                self.finished = true;
                drop(self.reader.take());
                Poll::Ready(Err(error))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl core::fmt::Debug for RpcPayloadReader {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RpcPayloadReader")
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl Drop for RpcPayloadReader {
    fn drop(&mut self) {
        drop(self.reader.take());
        self.shared
            .drop_side(PayloadSide::Reader(self.branch_index));
        self.finished = true;
    }
}

/// One independently readable response branch of a multicast RPC.
#[derive(Getters, MutGetters)]
pub struct RpcMulticastBranch {
    /// Endpoint address represented by this branch.
    #[getset(get = "pub")]
    address: RpcAddress,
    /// Branch's independent response reader.
    #[getset(get_mut = "pub")]
    reader: RpcPayloadReader,
}

impl RpcMulticastBranch {
    /// Consumes the branch and returns its response reader.
    #[must_use]
    pub fn into_reader(self) -> RpcPayloadReader {
        self.reader
    }
}

impl core::fmt::Debug for RpcMulticastBranch {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RpcMulticastBranch")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

pub(crate) fn make_payload_call(prepared: PreparedCalls) -> (RpcPayloadWriter, RpcPayloadReader) {
    let frame_capacity = prepared.request_frame_size();
    let shared = Rc::new(PayloadCallShared::new(prepared, 1));
    (
        RpcPayloadWriter {
            shared: Rc::clone(&shared),
            writer: None,
            frame_capacity,
            closed: false,
        },
        RpcPayloadReader {
            shared,
            input_driver: None,
            branch_index: 0,
            reader: None,
            finished: false,
        },
    )
}

pub(crate) fn make_payload_calls(
    prepared: PreparedCalls,
    addresses: Vec<RpcAddress>,
) -> (RpcPayloadWriter, Vec<RpcMulticastBranch>) {
    let frame_capacity = prepared.request_frame_size();
    let branch_count = addresses.len();
    let shared = Rc::new(PayloadCallShared::new(prepared, branch_count));
    let writer = RpcPayloadWriter {
        shared: Rc::clone(&shared),
        writer: None,
        frame_capacity,
        closed: false,
    };
    let branches = addresses
        .into_iter()
        .enumerate()
        .map(|(branch_index, address)| RpcMulticastBranch {
            address,
            reader: RpcPayloadReader {
                shared: Rc::clone(&shared),
                input_driver: None,
                branch_index,
                reader: None,
                finished: false,
            },
        })
        .collect();
    (writer, branches)
}

pub(crate) fn attach_input_driver(branches: &mut [RpcMulticastBranch], input: RpcFuture<'static>) {
    let driver = Rc::new(RefCell::new(PayloadInputDriver {
        input: Some(input),
        error: None,
    }));
    for branch in branches {
        branch.reader.input_driver = Some(Rc::clone(&driver));
    }
}
