use alloc::vec::Vec;
use core::cell::{Ref, RefCell, RefMut};
use core::future::Future;
use core::mem::align_of;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use getset::CopyGetters;
use smallvec::SmallVec;

use super::{RpcDirection, RpcError, RpcResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaneOwner {
    Free,
    Reserved(u64),
    Active,
}

#[repr(C, align(16))]
struct AlignedFrame<const M: usize> {
    bytes: [u8; M],
}

pub(crate) const LANE_FRAME_ALIGNMENT: usize = align_of::<AlignedFrame<0>>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaneFrameKind {
    Message,
    MethodError,
}

type BorrowedLaneFrame = (Ref<'static, [u8]>, LaneFrameKind);
type BorrowedLaneFramePoll = Poll<RpcResult<Option<BorrowedLaneFrame>>>;
type WakeList = SmallVec<[Waker; 1]>;
type LaneIndices = SmallVec<[usize; 1]>;
type LaneReaderSet = SmallVec<[LaneReader; 1]>;

impl<const M: usize> AlignedFrame<M> {
    const fn new() -> Self {
        Self { bytes: [0; M] }
    }
}

struct StaticPipeState {
    frame: PipeFrameState,
    readers: Vec<PipeReaderState>,
    writer_open: bool,
    writer_waker: Option<Waker>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipeFrameState {
    Empty,
    Reserved,
    Ready { len: usize, kind: LaneFrameKind },
}

struct PipeReaderState {
    open: bool,
    frame: PipeReaderFrameState,
    waker: Option<Waker>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipeReaderFrameState {
    Waiting,
    Borrowed,
    Consumed,
}

impl PipeReaderState {
    const fn new() -> Self {
        Self {
            open: true,
            frame: PipeReaderFrameState::Consumed,
            waker: None,
        }
    }
}

impl StaticPipeState {
    const fn new() -> Self {
        Self {
            frame: PipeFrameState::Empty,
            readers: Vec::new(),
            writer_open: true,
            writer_waker: None,
        }
    }
}

struct StaticPipe<const M: usize> {
    bytes: RefCell<AlignedFrame<M>>,
    state: RefCell<StaticPipeState>,
}

impl<const M: usize> StaticPipe<M> {
    const fn new() -> Self {
        Self {
            bytes: RefCell::new(AlignedFrame::new()),
            state: RefCell::new(StaticPipeState::new()),
        }
    }

    fn reset(&self) {
        let mut state = self.state.borrow_mut();
        state.frame = PipeFrameState::Empty;
        state.readers.clear();
        state.readers.push(PipeReaderState::new());
        state.writer_open = true;
        state.writer_waker = None;
    }

    fn configure_readers(&self, count: usize) -> RpcResult<()> {
        if count == 0 {
            return Err(RpcError::InvalidLaneState);
        }
        let mut state = self.state.borrow_mut();
        if state.frame != PipeFrameState::Empty {
            return Err(RpcError::InvalidLaneState);
        }
        state.readers.clear();
        state.readers.resize_with(count, PipeReaderState::new);
        Ok(())
    }

    fn poll_reserve_frame(
        &'static self,
        context: &mut Context<'_>,
    ) -> Poll<RpcResult<RefMut<'static, [u8]>>> {
        let mut state = self.state.borrow_mut();
        if !state.writer_open {
            return Poll::Ready(Err(RpcError::FrameWriterClosed));
        }
        if !state.readers.iter().any(|reader| reader.open) {
            return Poll::Ready(Err(RpcError::FrameReaderClosed));
        }
        if state.frame != PipeFrameState::Empty {
            update_waker(&mut state.writer_waker, context.waker());
            return Poll::Pending;
        }

        let bytes = match self.bytes.try_borrow_mut() {
            Ok(bytes) => RefMut::map(bytes, |frame| &mut frame.bytes[..]),
            Err(_) => return Poll::Ready(Err(RpcError::InvalidFrameState)),
        };
        state.frame = PipeFrameState::Reserved;
        Poll::Ready(Ok(bytes))
    }

    fn commit_reserved_frame(
        &self,
        length: usize,
        kind: LaneFrameKind,
    ) -> (RpcResult<()>, WakeList) {
        let mut state = self.state.borrow_mut();
        if state.frame != PipeFrameState::Reserved {
            return (Err(RpcError::InvalidFrameState), WakeList::new());
        }
        if !state.writer_open {
            return (Err(RpcError::FrameWriterClosed), WakeList::new());
        }
        if !state.readers.iter().any(|reader| reader.open) {
            return (Err(RpcError::FrameReaderClosed), WakeList::new());
        }
        if length > M {
            return (
                Err(RpcError::FrameTooLarge {
                    size: length,
                    capacity: M,
                }),
                WakeList::new(),
            );
        }

        state.frame = PipeFrameState::Ready { len: length, kind };
        let mut wakers = WakeList::new();
        for reader in state.readers.iter_mut().filter(|reader| reader.open) {
            reader.frame = PipeReaderFrameState::Waiting;
            if let Some(waker) = reader.waker.take() {
                wakers.push(waker);
            }
        }
        (Ok(()), wakers)
    }

    fn cancel_reserved_frame(&self) -> Option<Waker> {
        let mut state = self.state.borrow_mut();
        if state.frame != PipeFrameState::Reserved {
            return None;
        }
        state.frame = PipeFrameState::Empty;
        state.writer_waker.take()
    }

    fn poll_borrow_frame(
        &'static self,
        reader_id: usize,
        context: &mut Context<'_>,
    ) -> BorrowedLaneFramePoll {
        let mut state = self.state.borrow_mut();
        let (length, kind) = match state.frame {
            PipeFrameState::Empty | PipeFrameState::Reserved => {
                if !state.writer_open && state.frame == PipeFrameState::Empty {
                    return Poll::Ready(Ok(None));
                }
                let Some(reader) = state.readers.get_mut(reader_id) else {
                    return Poll::Ready(Err(RpcError::InvalidLaneState));
                };
                update_waker(&mut reader.waker, context.waker());
                return Poll::Pending;
            }
            PipeFrameState::Ready { len, kind } => (len, kind),
        };
        let Some(reader) = state.readers.get_mut(reader_id) else {
            return Poll::Ready(Err(RpcError::InvalidLaneState));
        };
        if !reader.open {
            return Poll::Ready(Ok(None));
        }
        if reader.frame != PipeReaderFrameState::Waiting {
            update_waker(&mut reader.waker, context.waker());
            return Poll::Pending;
        }
        reader.frame = PipeReaderFrameState::Borrowed;
        drop(state);

        match Ref::filter_map(self.bytes.borrow(), |frame| frame.bytes.get(..length)) {
            Ok(bytes) => Poll::Ready(Ok(Some((bytes, kind)))),
            Err(_) => {
                let mut state = self.state.borrow_mut();
                if let Some(reader) = state.readers.get_mut(reader_id) {
                    reader.frame = PipeReaderFrameState::Waiting;
                }
                Poll::Ready(Err(RpcError::InvalidFrameState))
            }
        }
    }

    fn release_borrowed_frame(&self, reader_id: usize) -> (Option<Waker>, WakeList) {
        let mut state = self.state.borrow_mut();
        let Some(reader) = state.readers.get_mut(reader_id) else {
            return (None, WakeList::new());
        };
        if reader.frame != PipeReaderFrameState::Borrowed {
            return (None, WakeList::new());
        }
        reader.frame = PipeReaderFrameState::Consumed;
        Self::finish_frame_if_consumed(&mut state)
    }

    fn close_writer(&self) -> WakeList {
        let mut state = self.state.borrow_mut();
        state.writer_open = false;
        state
            .readers
            .iter_mut()
            .filter_map(|reader| reader.waker.take())
            .collect()
    }

    fn close_reader(&self, reader_id: usize) -> Option<Waker> {
        let mut state = self.state.borrow_mut();
        let reader = state.readers.get_mut(reader_id)?;
        reader.open = false;
        reader.waker = None;
        Self::finish_frame_if_consumed(&mut state).0
    }

    fn finish_frame_if_consumed(state: &mut StaticPipeState) -> (Option<Waker>, WakeList) {
        let complete = matches!(state.frame, PipeFrameState::Ready { .. })
            && state.readers.iter().all(|reader| {
                reader.frame == PipeReaderFrameState::Consumed
                    || (!reader.open && reader.frame != PipeReaderFrameState::Borrowed)
            });
        if !complete {
            return (None, WakeList::new());
        }
        state.frame = PipeFrameState::Empty;
        let writer = state.writer_waker.take();
        let readers = state
            .readers
            .iter_mut()
            .filter_map(|reader| reader.waker.take())
            .collect();
        (writer, readers)
    }
}

struct LaneState {
    owner: LaneOwner,
    handles: usize,
}

impl LaneState {
    const fn new() -> Self {
        Self {
            owner: LaneOwner::Free,
            handles: 0,
        }
    }

    fn activate(&mut self) {
        self.owner = LaneOwner::Active;
        self.handles = 4;
    }
}

struct WaiterState {
    ticket: Option<u64>,
    requested: usize,
    waker: Option<Waker>,
}

impl WaiterState {
    const fn new() -> Self {
        Self {
            ticket: None,
            requested: 0,
            waker: None,
        }
    }
}

struct PoolState<const N: usize, const Q: usize> {
    lanes: [LaneState; N],
    waiters: [WaiterState; Q],
    next_ticket: u64,
}

impl<const N: usize, const Q: usize> PoolState<N, Q> {
    const fn new() -> Self {
        Self {
            lanes: [const { LaneState::new() }; N],
            waiters: [const { WaiterState::new() }; Q],
            next_ticket: 1,
        }
    }

    fn free_lane_count(&self) -> usize {
        self.lanes
            .iter()
            .filter(|lane| lane.owner == LaneOwner::Free)
            .count()
    }

    fn reserved_lane_count(&self, ticket: u64) -> usize {
        self.lanes
            .iter()
            .filter(|lane| lane.owner == LaneOwner::Reserved(ticket))
            .count()
    }

    fn waiter_index(&self, ticket: u64) -> Option<usize> {
        self.waiters
            .iter()
            .position(|waiter| waiter.ticket == Some(ticket))
    }

    fn next_waiter_index(&self) -> Option<usize> {
        self.waiters
            .iter()
            .enumerate()
            .filter_map(|(index, waiter)| waiter.ticket.map(|ticket| (index, ticket)))
            .min_by_key(|(_, ticket)| *ticket)
            .map(|(index, _)| index)
    }

    fn activate_lane(&mut self, index: usize) -> RpcResult<()> {
        let lane = self
            .lanes
            .get_mut(index)
            .ok_or(RpcError::InvalidLaneState)?;
        lane.activate();
        Ok(())
    }

    fn clear_waiter(&mut self, ticket: u64) -> RpcResult<Option<Waker>> {
        let Some(index) = self.waiter_index(ticket) else {
            return Ok(None);
        };
        let waiter = self
            .waiters
            .get_mut(index)
            .ok_or(RpcError::InvalidLaneState)?;
        waiter.ticket = None;
        waiter.requested = 0;
        Ok(waiter.waker.take())
    }

    fn reserve_for_next_waiter(&mut self) -> RpcResult<Option<Waker>> {
        let Some(waiter_index) = self.next_waiter_index() else {
            return Ok(None);
        };
        let (ticket, requested) = {
            let waiter = self
                .waiters
                .get(waiter_index)
                .ok_or(RpcError::InvalidLaneState)?;
            (
                waiter.ticket.ok_or(RpcError::InvalidLaneState)?,
                waiter.requested,
            )
        };
        if self.reserved_lane_count(ticket) != 0 {
            return Ok(None);
        }
        if self.free_lane_count() < requested {
            return Ok(None);
        }
        let mut remaining = requested;
        for lane in self
            .lanes
            .iter_mut()
            .filter(|lane| lane.owner == LaneOwner::Free)
        {
            if remaining == 0 {
                break;
            }
            lane.owner = LaneOwner::Reserved(ticket);
            remaining = remaining.saturating_sub(1);
        }
        if remaining != 0 {
            return Err(RpcError::InvalidLaneState);
        }
        Ok(self
            .waiters
            .get_mut(waiter_index)
            .ok_or(RpcError::InvalidLaneState)?
            .waker
            .take())
    }
}

/// Fixed-capacity storage for active RPC calls and lane waiters.
///
/// Each lane owns one 16-byte-aligned `M`-byte request pipe and one aligned
/// `M`-byte response pipe. Response-pipe metadata tags each payload as either a
/// successful response or a method error without adding a payload header.
/// `N` therefore reserves approximately `N * 2 * M` payload bytes. A returned
/// [`RpcFrame`](super::RpcFrame) or [`RpcPayloadFrame`](super::RpcPayloadFrame)
/// retains its pipe and lane until drop. `Q` bounds the number of root calls
/// that may wait for a lane. The storage must have application lifetime and is
/// intended for the registry's one cooperative executor thread.
pub struct RpcLaneStorage<const N: usize, const M: usize, const Q: usize> {
    state: RefCell<PoolState<N, Q>>,
    requests: [StaticPipe<M>; N],
    responses: [StaticPipe<M>; N],
}

impl<const N: usize, const M: usize, const Q: usize> RpcLaneStorage<N, M, Q> {
    /// Creates zero-initialized fixed-capacity lane storage.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: RefCell::new(PoolState::new()),
            requests: [const { StaticPipe::new() }; N],
            responses: [const { StaticPipe::new() }; N],
        }
    }

    fn pipe(&self, lane: usize, direction: RpcDirection) -> Option<&StaticPipe<M>> {
        match direction {
            RpcDirection::Request => self.requests.get(lane),
            RpcDirection::Response => self.responses.get(lane),
        }
    }

    fn reset_lane(&self, lane: usize) -> RpcResult<()> {
        let request = self.requests.get(lane).ok_or(RpcError::InvalidLaneState)?;
        let response = self.responses.get(lane).ok_or(RpcError::InvalidLaneState)?;
        request.reset();
        response.reset();
        Ok(())
    }
}

impl<const N: usize, const M: usize, const Q: usize> Default for RpcLaneStorage<N, M, Q> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(CopyGetters)]
pub(crate) struct BorrowedFrame {
    bytes: Ref<'static, [u8]>,
    _lease: BorrowedFrameLease,
    #[getset(get_copy = "pub(crate)")]
    kind: LaneFrameKind,
}

struct BorrowedFrameLease {
    pool: &'static dyn LanePool,
    lane: usize,
    direction: RpcDirection,
    reader_id: usize,
}

impl BorrowedFrame {
    fn new(
        bytes: Ref<'static, [u8]>,
        pool: &'static dyn LanePool,
        lane: usize,
        direction: RpcDirection,
        reader_id: usize,
        kind: LaneFrameKind,
    ) -> Self {
        Self {
            bytes,
            _lease: BorrowedFrameLease {
                pool,
                lane,
                direction,
                reader_id,
            },
            kind,
        }
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for BorrowedFrameLease {
    fn drop(&mut self) {
        self.pool
            .release_borrowed_frame(self.lane, self.direction, self.reader_id);
        self.pool.release_handle(self.lane);
    }
}

pub(crate) struct ReservedFrame {
    bytes: RefMut<'static, [u8]>,
    reservation: FrameReservation,
}

impl ReservedFrame {
    fn new(
        bytes: RefMut<'static, [u8]>,
        pool: &'static dyn LanePool,
        lane: usize,
        direction: RpcDirection,
    ) -> Self {
        Self {
            bytes,
            reservation: FrameReservation {
                pool,
                lane,
                direction,
                active: true,
            },
        }
    }

    pub(crate) fn commit(self, length: usize, kind: LaneFrameKind) -> RpcResult<()> {
        let Self {
            bytes,
            mut reservation,
        } = self;
        let capacity = bytes.len();
        if length > capacity {
            return Err(RpcError::FrameTooLarge {
                size: length,
                capacity,
            });
        }
        drop(bytes);
        reservation.commit(length, kind)
    }
}

impl AsMut<[u8]> for ReservedFrame {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

impl AsRef<[u8]> for ReservedFrame {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl ReservedFrame {
    pub(crate) fn limit(self, limit: usize) -> RpcResult<Self> {
        let Self { bytes, reservation } = self;
        let capacity = bytes.len();
        match RefMut::filter_map(bytes, |buffer| buffer.get_mut(..limit)) {
            Ok(bytes) => Ok(Self { bytes, reservation }),
            Err(bytes) => {
                drop(bytes);
                Err(RpcError::FrameTooLarge {
                    size: limit,
                    capacity,
                })
            }
        }
    }
}

struct FrameReservation {
    pool: &'static dyn LanePool,
    lane: usize,
    direction: RpcDirection,
    active: bool,
}

impl FrameReservation {
    fn commit(&mut self, length: usize, kind: LaneFrameKind) -> RpcResult<()> {
        self.pool
            .commit_reserved_frame(self.lane, self.direction, length, kind)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for FrameReservation {
    fn drop(&mut self) {
        if self.active {
            self.pool.cancel_reserved_frame(self.lane, self.direction);
        }
    }
}

pub(crate) trait LanePool {
    fn poll_acquire_many(
        &self,
        token: &mut Option<u64>,
        nested: bool,
        count: usize,
        context: &mut Context<'_>,
    ) -> Poll<RpcResult<LaneIndices>>;
    fn cancel_waiter(&self, token: u64);
    fn poll_borrow_frame(
        &'static self,
        lane: usize,
        direction: RpcDirection,
        reader_id: usize,
        context: &mut Context<'_>,
    ) -> Poll<RpcResult<Option<BorrowedFrame>>>;
    fn poll_reserve_frame(
        &'static self,
        lane: usize,
        direction: RpcDirection,
        context: &mut Context<'_>,
    ) -> Poll<RpcResult<ReservedFrame>>;
    fn commit_reserved_frame(
        &self,
        lane: usize,
        direction: RpcDirection,
        length: usize,
        kind: LaneFrameKind,
    ) -> RpcResult<()>;
    fn cancel_reserved_frame(&self, lane: usize, direction: RpcDirection);
    fn configure_readers(
        &self,
        lane: usize,
        direction: RpcDirection,
        count: usize,
    ) -> RpcResult<()>;
    fn close_reader(&self, lane: usize, direction: RpcDirection, reader_id: usize);
    fn close_writer(&self, lane: usize, direction: RpcDirection);
    fn release_borrowed_frame(&self, lane: usize, direction: RpcDirection, reader_id: usize);
    fn retain_handle(&self, lane: usize) -> RpcResult<()>;
    fn release_handle(&self, lane: usize);
}

impl<const N: usize, const M: usize, const Q: usize> LanePool for RpcLaneStorage<N, M, Q> {
    fn poll_acquire_many(
        &self,
        token: &mut Option<u64>,
        nested: bool,
        count: usize,
        context: &mut Context<'_>,
    ) -> Poll<RpcResult<LaneIndices>> {
        if count == 0 || count > N {
            return Poll::Ready(Err(RpcError::LaneBatchTooLarge {
                requested: count,
                limit: N,
            }));
        }
        let mut state = self.state.borrow_mut();
        if let Some(ticket) = *token {
            if state.reserved_lane_count(ticket) == count {
                let indices: LaneIndices = state
                    .lanes
                    .iter()
                    .enumerate()
                    .filter_map(|(index, lane)| {
                        (lane.owner == LaneOwner::Reserved(ticket)).then_some(index)
                    })
                    .collect();
                if let Err(error) = state.clear_waiter(ticket) {
                    return Poll::Ready(Err(error));
                }
                for &index in &indices {
                    if let Err(error) = state.activate_lane(index) {
                        return Poll::Ready(Err(error));
                    }
                }
                *token = None;
                drop(state);
                for &index in &indices {
                    if let Err(error) = self.reset_lane(index) {
                        return Poll::Ready(Err(error));
                    }
                }
                return Poll::Ready(Ok(indices));
            }
            let Some(waiter_index) = state.waiter_index(ticket) else {
                return Poll::Ready(Err(RpcError::InvalidLaneState));
            };
            let Some(waiter) = state.waiters.get_mut(waiter_index) else {
                return Poll::Ready(Err(RpcError::InvalidLaneState));
            };
            update_waker(&mut waiter.waker, context.waker());
            return Poll::Pending;
        }

        if state.next_waiter_index().is_none() && state.free_lane_count() >= count {
            let indices: LaneIndices = state
                .lanes
                .iter()
                .enumerate()
                .filter_map(|(index, lane)| (lane.owner == LaneOwner::Free).then_some(index))
                .take(count)
                .collect();
            for &index in &indices {
                if let Err(error) = state.activate_lane(index) {
                    return Poll::Ready(Err(error));
                }
            }
            drop(state);
            for &index in &indices {
                if let Err(error) = self.reset_lane(index) {
                    return Poll::Ready(Err(error));
                }
            }
            return Poll::Ready(Ok(indices));
        }
        if nested {
            return Poll::Ready(Err(RpcError::NestedLaneExhausted { limit: N }));
        }
        let Some(waiter_index) = state
            .waiters
            .iter()
            .position(|waiter| waiter.ticket.is_none())
        else {
            return Poll::Ready(Err(RpcError::LaneWaiterCapacityExceeded { limit: Q }));
        };
        let ticket = state.next_ticket;
        let Some(next_ticket) = ticket.checked_add(1) else {
            return Poll::Ready(Err(RpcError::IdentifierExhausted));
        };
        state.next_ticket = next_ticket;
        let Some(waiter) = state.waiters.get_mut(waiter_index) else {
            return Poll::Ready(Err(RpcError::InvalidLaneState));
        };
        waiter.ticket = Some(ticket);
        waiter.requested = count;
        update_waker(&mut waiter.waker, context.waker());
        *token = Some(ticket);
        Poll::Pending
    }

    fn cancel_waiter(&self, token: u64) {
        let waker = {
            let mut state = self.state.borrow_mut();
            let _removed_waker = state.clear_waiter(token).ok().flatten();
            for lane in state
                .lanes
                .iter_mut()
                .filter(|lane| lane.owner == LaneOwner::Reserved(token))
            {
                lane.owner = LaneOwner::Free;
            }
            state.reserve_for_next_waiter().ok().flatten()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn poll_borrow_frame(
        &'static self,
        lane: usize,
        direction: RpcDirection,
        reader_id: usize,
        context: &mut Context<'_>,
    ) -> Poll<RpcResult<Option<BorrowedFrame>>> {
        let Some(pipe) = self.pipe(lane, direction) else {
            return Poll::Ready(Err(RpcError::InvalidLaneState));
        };
        match pipe.poll_borrow_frame(reader_id, context) {
            Poll::Ready(Ok(Some((bytes, kind)))) => {
                if let Err(error) = self.retain_handle(lane) {
                    drop(bytes);
                    self.release_borrowed_frame(lane, direction, reader_id);
                    return Poll::Ready(Err(error));
                }
                Poll::Ready(Ok(Some(BorrowedFrame::new(
                    bytes, self, lane, direction, reader_id, kind,
                ))))
            }
            Poll::Ready(Ok(None)) => Poll::Ready(Ok(None)),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_reserve_frame(
        &'static self,
        lane: usize,
        direction: RpcDirection,
        context: &mut Context<'_>,
    ) -> Poll<RpcResult<ReservedFrame>> {
        let Some(pipe) = self.pipe(lane, direction) else {
            return Poll::Ready(Err(RpcError::InvalidLaneState));
        };
        match pipe.poll_reserve_frame(context) {
            Poll::Ready(Ok(bytes)) => {
                Poll::Ready(Ok(ReservedFrame::new(bytes, self, lane, direction)))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn commit_reserved_frame(
        &self,
        lane: usize,
        direction: RpcDirection,
        length: usize,
        kind: LaneFrameKind,
    ) -> RpcResult<()> {
        let Some(pipe) = self.pipe(lane, direction) else {
            return Err(RpcError::InvalidLaneState);
        };
        let (result, wakers) = pipe.commit_reserved_frame(length, kind);
        for waker in wakers {
            waker.wake();
        }
        result
    }

    fn cancel_reserved_frame(&self, lane: usize, direction: RpcDirection) {
        let waker = self
            .pipe(lane, direction)
            .and_then(StaticPipe::cancel_reserved_frame);
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn configure_readers(
        &self,
        lane: usize,
        direction: RpcDirection,
        count: usize,
    ) -> RpcResult<()> {
        self.pipe(lane, direction)
            .ok_or(RpcError::InvalidLaneState)?
            .configure_readers(count)
    }

    fn close_reader(&self, lane: usize, direction: RpcDirection, reader_id: usize) {
        let waker = self
            .pipe(lane, direction)
            .and_then(|pipe| pipe.close_reader(reader_id));
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn close_writer(&self, lane: usize, direction: RpcDirection) {
        let wakers = self
            .pipe(lane, direction)
            .map(StaticPipe::close_writer)
            .unwrap_or_default();
        for waker in wakers {
            waker.wake();
        }
    }

    fn release_borrowed_frame(&self, lane: usize, direction: RpcDirection, reader_id: usize) {
        let Some(pipe) = self.pipe(lane, direction) else {
            return;
        };
        let (writer_waker, reader_wakers) = pipe.release_borrowed_frame(reader_id);
        if let Some(waker) = writer_waker {
            waker.wake();
        }
        for waker in reader_wakers {
            waker.wake();
        }
    }

    fn retain_handle(&self, lane_index: usize) -> RpcResult<()> {
        let mut state = self.state.borrow_mut();
        let lane = state
            .lanes
            .get_mut(lane_index)
            .ok_or(RpcError::InvalidLaneState)?;
        if lane.owner != LaneOwner::Active || lane.handles == 0 {
            return Err(RpcError::InvalidLaneState);
        }
        lane.handles = lane
            .handles
            .checked_add(1)
            .ok_or(RpcError::InvalidLaneState)?;
        Ok(())
    }

    fn release_handle(&self, lane_index: usize) {
        let waker = {
            let mut state = self.state.borrow_mut();
            let released = match state.lanes.get_mut(lane_index) {
                Some(lane) if lane.handles > 0 => {
                    lane.handles = lane.handles.saturating_sub(1);
                    lane.handles == 0
                }
                _ => false,
            };
            if released {
                if let Some(lane) = state.lanes.get_mut(lane_index) {
                    lane.owner = LaneOwner::Free;
                }
                state.reserve_for_next_waiter().ok().flatten()
            } else {
                None
            }
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

pub(crate) struct LaneAcquireSet {
    pool: &'static dyn LanePool,
    nested: bool,
    count: usize,
    token: Option<u64>,
    finished: bool,
}

impl LaneAcquireSet {
    pub(crate) fn new(pool: &'static dyn LanePool, nested: bool, count: usize) -> Self {
        Self {
            pool,
            nested,
            count,
            token: None,
            finished: false,
        }
    }
}

impl Future for LaneAcquireSet {
    type Output = RpcResult<LaneIoSet>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(Err(RpcError::CompletedCallPolled));
        }
        match this
            .pool
            .poll_acquire_many(&mut this.token, this.nested, this.count, context)
        {
            Poll::Ready(Ok(indices)) => {
                this.finished = true;
                Poll::Ready(Ok(indices
                    .into_iter()
                    .map(|index| LaneIo::new(this.pool, index))
                    .collect()))
            }
            Poll::Ready(Err(error)) => {
                this.finished = true;
                Poll::Ready(Err(error))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for LaneAcquireSet {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.pool.cancel_waiter(token);
        }
    }
}

pub(crate) type LaneIoSet = SmallVec<[LaneIo; 1]>;

pub(crate) struct LaneIo {
    pub(crate) request_reader: Option<LaneReader>,
    pub(crate) request_writer: Option<LaneWriter>,
    pub(crate) response_reader: LaneReader,
    pub(crate) response_writer: LaneWriter,
}

impl LaneIo {
    fn new(pool: &'static dyn LanePool, lane: usize) -> Self {
        Self {
            request_reader: Some(LaneReader::new(pool, lane, RpcDirection::Request)),
            request_writer: Some(LaneWriter::new(pool, lane, RpcDirection::Request)),
            response_reader: LaneReader::new(pool, lane, RpcDirection::Response),
            response_writer: LaneWriter::new(pool, lane, RpcDirection::Response),
        }
    }

    pub(crate) fn share_request_readers(&mut self, count: usize) -> RpcResult<LaneReaderSet> {
        let request_reader = self
            .request_reader
            .take()
            .ok_or(RpcError::InvalidLaneState)?;
        let pool = request_reader.pool;
        let lane = request_reader.lane;
        pool.configure_readers(lane, RpcDirection::Request, count)?;
        let mut readers = LaneReaderSet::with_capacity(count);
        readers.push(request_reader);
        for reader_id in 1..count {
            readers.push(LaneReader::additional(
                pool,
                lane,
                RpcDirection::Request,
                reader_id,
            )?);
        }
        Ok(readers)
    }
}

pub(crate) struct LaneReader {
    pool: &'static dyn LanePool,
    lane: usize,
    direction: RpcDirection,
    reader_id: usize,
}

impl LaneReader {
    fn new(pool: &'static dyn LanePool, lane: usize, direction: RpcDirection) -> Self {
        Self {
            pool,
            lane,
            direction,
            reader_id: 0,
        }
    }

    fn additional(
        pool: &'static dyn LanePool,
        lane: usize,
        direction: RpcDirection,
        reader_id: usize,
    ) -> RpcResult<Self> {
        pool.retain_handle(lane)?;
        Ok(Self {
            pool,
            lane,
            direction,
            reader_id,
        })
    }

    pub(crate) fn poll_borrow_frame(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<RpcResult<Option<BorrowedFrame>>> {
        self.pool
            .poll_borrow_frame(self.lane, self.direction, self.reader_id, context)
    }
}

impl Drop for LaneReader {
    fn drop(&mut self) {
        self.pool
            .close_reader(self.lane, self.direction, self.reader_id);
        self.pool.release_handle(self.lane);
    }
}

pub(crate) struct LaneWriter {
    pool: &'static dyn LanePool,
    lane: usize,
    direction: RpcDirection,
}

impl LaneWriter {
    fn new(pool: &'static dyn LanePool, lane: usize, direction: RpcDirection) -> Self {
        Self {
            pool,
            lane,
            direction,
        }
    }

    pub(crate) fn poll_reserve_frame(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<RpcResult<ReservedFrame>> {
        self.pool
            .poll_reserve_frame(self.lane, self.direction, context)
    }
}

impl Drop for LaneWriter {
    fn drop(&mut self) {
        self.pool.close_writer(self.lane, self.direction);
        self.pool.release_handle(self.lane);
    }
}

fn update_waker(slot: &mut Option<Waker>, waker: &Waker) {
    if slot
        .as_ref()
        .is_none_or(|registered| !registered.will_wake(waker))
    {
        *slot = Some(waker.clone());
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::boxed::Box;
    use futures_lite::future::{block_on, poll_once};

    use super::*;

    #[test]
    fn batch_acquisition_waits_for_all_lanes_without_loaning_free_lane_out_of_order() {
        block_on(async {
            let storage = Box::leak(Box::new(RpcLaneStorage::<2, 16, 2>::new()));
            let pool: &'static dyn LanePool = storage;
            let held = LaneAcquireSet::new(pool, false, 1)
                .await
                .expect("acquire one lane");

            let mut batch = LaneAcquireSet::new(pool, false, 2);
            assert!(poll_once(&mut batch).await.is_none());

            let mut later_single = LaneAcquireSet::new(pool, false, 1);
            assert!(poll_once(&mut later_single).await.is_none());

            drop(held);
            let batch_lanes = batch.await.expect("atomically acquire both lanes");
            assert_eq!(batch_lanes.len(), 2);
            assert!(poll_once(&mut later_single).await.is_none());

            drop(batch_lanes);
            let single_lane = later_single.await.expect("acquire after older batch");
            assert_eq!(single_lane.len(), 1);
        });
    }
}
