# Chip drivers

## Purpose

`drivers/chips` contains low-level, reusable register and protocol drivers for
physical chips. A chip driver exposes operations in the chip's vocabulary and
consumes upstream bus and GPIO traits. It does not decide what the chip means to
a Barracuda application.

Chip drivers are private building blocks. They do not appear in `board.yml`, do
not own `peripheral.yml`, and are not discovered by `cargo board`. A semantic
peripheral implementation selects them through an ordinary Cargo dependency.

Vendor register tables and the build-time parsers that normalize them belong
with chip drivers as well. Shared parser code may live in `drivers/build-support`;
it runs only while building a chip crate and never becomes a runtime dependency.

## Dependency boundary

A chip driver may depend on `embedded-hal`, `embedded-hal-async`, or a focused
upstream protocol crate. It must not depend on:

- `peripherals/api` or `peripherals/config`;
- Board, Target, System, or Plugin crates;
- a vendor HAL;
- generated Board wiring or a concrete Board name.

The consumer supplies addresses, pins, delays, and bus values. The driver
validates chip-level inputs and reports chip or transport errors without
inventing application semantics.

## Composition

One semantic implementation may combine several chip drivers. Conversely, the
same chip driver may support several semantic implementations. For example,
Tab5 peripheral implementations independently borrow the shared I2C resource
and use the PI4IOE5V6408 driver for the reset or enable signal they own. The
expander itself never becomes a System-visible peripheral.

This separation is intentional even when the low-level code is small: it keeps
register behavior reusable and prevents helper chips from leaking into the
Board's semantic API.

MIPI-DSI panel drivers expose controller-native command sequences. The
display-peripheral implementation chooses the panel and timing, then forwards
those commands through the Platform's narrow DSI command channel. Neither the
Platform nor the semantic display API owns a vendor register table.
