//! Chip drivers checked against datasheet models of their chips.
//!
//! The tests live in `tests/`, one file per chip. They drive each driver over
//! the host-only virtual buses of `barracuda-platform-virtual-io`, whose chip
//! models check register use, command order and timing against the chip's
//! datasheet. Keeping them here leaves the chip driver crates free of any
//! Platform dependency.
