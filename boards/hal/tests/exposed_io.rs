//! Ownership and naming behavior of explicitly exposed Board I/O.

#![allow(clippy::expect_used)]

use barracuda_board_hal::{ExposedIo, NoExposedIo};

#[test]
fn exposed_io_is_one_shared_runtime_owner() {
    fn assert_exposed_io<T: ExposedIo>() {}

    assert_exposed_io::<NoExposedIo>();
}

#[test]
fn empty_io_remains_a_typed_runtime_owner() {
    let io = NoExposedIo;
    let _copy = io;
}
