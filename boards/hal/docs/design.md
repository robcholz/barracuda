# Exposed I/O Design

## Scope

Exposed I/O lets an application select a function for Board-visible physical
resources at runtime. A package pin is the stable resource; digital GPIO,
analog conversion, PWM, I2C, SPI, UART, and I2S are functions constructed from
move-only pin and controller tokens.

Built-in peripherals are outside this runtime model. Board composition
constructs their peripheral implementations before System starts and passes only the remaining,
explicitly exposed resources to the runtime I/O owner.

```text
selected Board pins + Platform controller pools
                 |
                 v
       one exposed-I/O owner
       +-------------------+
       | move-only tokens  |
       | Platform providers|
       +-------------------+
          |       |       |
          v       v       v
       vm-gpio  vm-i2c  vm-spi  ...
          |       |       |
          `-------+-------'
                  v
            Lua HAL handles
```

## Physical resources and function claims

The generated owner stores each exposed pin token once under its Board-visible
application name. Compatible I2C and SPI controller tokens come from Platform
metadata; their identities remain invisible to applications. Controllers
already consumed by built-in declarations are removed from the runtime pools
during generation.

A provider resolves every role in one request and claims the complete set
atomically. A digital output consumes one pin. An I2C bus consumes a controller,
SCL, and SDA. An I2S transmitter may consume a controller, BCLK, WS, DOUT, and
DMA. Duplicate roles, unknown names, exhausted controllers, and already claimed
pins fail before any token moves.

The first successful function selection consumes those raw tokens for the
current boot. Dropping a handle disables or drops the constructed HAL value,
but it cannot safely recreate the original vendor singleton tokens. Reopening
that pin, including as another protocol, therefore fails until restart. This is
an intentional safety boundary rather than an emulated hot-reconfiguration
promise. Concurrent electrical fan-out and implicit pin sharing require a
separate Platform resource.

## Provider boundary

Protocol construction is expressed as one provider trait per function family.
The current contracts construct digital pins, asynchronous I2C buses, and
asynchronous SPI buses. System requires these providers on one exposed-I/O
owner, so every protocol sees the same physical token state.

The Platform implementation validates pin routing and configuration and then
returns a concrete value implementing the ecosystem contract. Digital handles
implement `embedded-hal` digital traits, I2C handles implement
`embedded-hal-async::i2c::I2c`, and SPI bus handles implement
`embedded-hal-async::spi::SpiBus`. `ConfigurableDigitalPin` covers the runtime
input/output mode transition that embedded-hal does not standardize. The
provider contract covers construction because embedded-hal intentionally does
not define pin mux or peripheral allocation; data-plane operations remain
upstream HAL operations.

The generated `RuntimeIo` is the single concrete owner. A Platform with runtime
controller pools makes I2C and SPI construction available; a Platform with
empty pools returns an explicit no-controller error. The VM packages therefore
never pretend a controller exists merely because pins were exposed.

New function families add a focused provider beside their VM package. UART can
return an `embedded-io-async` stream, PWM can return
`embedded-hal::pwm::SetDutyCycle`, and a domain without an upstream contract
can define a narrow domain interface. Each provider extends the same move-only
owner rather than creating a protocol-local resource map.

## Application call path

System shares one exposed-I/O owner with every hardware VM Plugin. A Plugin
opens a complete function and places the returned concrete value in a Lua
userdata handle. Operations are methods on that handle, so capability lifetime
and hardware lifetime are the same object.

```text
i2c.open(scl, sda, frequency)
  -> validate Lua values
  -> provider resolves pins and selects a compatible free controller
  -> atomically claim controller + SCL + SDA
  -> Platform configures pin mux and controller
  -> return I2C handle implementing embedded-hal-async
  -> handle:read/write/write_read
  -> close, lexical <close>, GC, or VM teardown
  -> disable/drop the HAL value; raw tokens remain claimed until restart
```

Lua packages validate protocol-independent facts such as positive frequencies,
distinct signal pins, transfer limits, address width, and SPI mode. Providers
validate Board exposure, ownership, controller availability, and Platform
hardware limits. Transaction failures are reported by the concrete HAL value.
A rejected pre-claim check leaves every requested resource unchanged. If a
vendor constructor itself rejects configuration after consuming its arguments,
those move-only tokens remain claimed; the error tells the application to
correct its configuration and restart.

Package revocation rejects new opens and operations through existing handles.
Explicit `close` and Lua's lexical `<close>` provide deterministic handle and
hardware teardown; GC is the final cleanup path when application code loses a
handle. None of these paths manufactures replacement vendor tokens.

## Application pressure

The public shape is checked against several materially different consumers:

| Application | Claimed resources | Returned operation contract |
| --- | --- | --- |
| button or relay | one pin | digital input/output |
| ADC probe | pin plus ADC route | analog sample |
| PWM actuator | pin plus timer/channel | duty-cycle output |
| I2C sensor bus | controller, SCL, SDA | async I2C transactions |
| SPI pixel stream | controller, SCK, data pins | exclusive async SPI bus |
| SPI register device | shared bus plus CS | async SPI device transactions |
| UART console | controller, TX/RX and optional flow-control pins | async byte stream |
| I2S audio | controller, clocks, data pins, DMA | PCM stream |

Single-pin, multi-wire, optional-role, shared-bus, and DMA-backed cases all use
the same atomic move-only claim primitive. Their different configuration and
operation semantics stay in providers and VM packages. This is the test for
both directions of abstraction: adding a protocol must not create an
independent ownership universe, and a package must not reconstruct Platform
pin mux itself.

## Invariants

- One generated exposed-I/O owner represents the complete runtime hardware
  surface of a selected Board.
- Every hardware VM Plugin shares that owner rather than taking an independent
  GPIO, I2C, or SPI collection.
- A physical token is consumed by at most one function during a boot.
- Multi-resource acquisition either claims every role or none of them.
- Platform routing and construction complete before a handle is returned.
- Function handles operate through upstream HAL traits where those traits
  exist.
- Closing or dropping a handle tears down the constructed HAL value without
  recreating raw tokens; revocation immediately prevents later hardware access.
