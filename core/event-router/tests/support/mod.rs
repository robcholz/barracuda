use core::future::Future;
use core::pin::Pin;
use std::fmt;
use std::task::{Context, Poll, Waker};

use barracuda_event_router::{EventRouter, RouterError};

#[derive(Debug)]
pub enum DriveError {
    Completed,
    Terminated(RouterError),
    PollLimit(usize),
}

impl fmt::Display for DriveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed => formatter.write_str("Event Router unexpectedly completed"),
            Self::Terminated(error) => {
                write!(formatter, "Event Router unexpectedly terminated: {error}")
            }
            Self::PollLimit(limit) => {
                write!(
                    formatter,
                    "Event Router exceeded the {limit}-poll test limit"
                )
            }
        }
    }
}

impl std::error::Error for DriveError {}

pub fn drive_until<const N: usize, const M: usize, const Q: usize>(
    event_router: &mut EventRouter<N, M, Q>,
    mut ready: impl FnMut(&EventRouter<N, M, Q>) -> bool,
) -> Result<usize, DriveError> {
    const POLL_LIMIT: usize = 1_000_000;

    let mut context = Context::from_waker(Waker::noop());
    for turn in 1..=POLL_LIMIT {
        match Pin::new(&mut *event_router).poll(&mut context) {
            Poll::Ready(Ok(())) => return Err(DriveError::Completed),
            Poll::Ready(Err(error)) => return Err(DriveError::Terminated(error)),
            Poll::Pending => {}
        }
        if ready(event_router) {
            return Ok(turn);
        }
    }
    Err(DriveError::PollLimit(POLL_LIMIT))
}
