"""Client and scenario support for the host Platform's virtual peripherals manager.

The manager speaks line-delimited JSON over loopback TCP; the protocol is
documented in `platforms/virtual-io/README.md`.
"""

from __future__ import annotations

import json
import socket
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

ADDRESS_VARIABLE = 'BARRACUDA_VIRTUAL_IO_ADDR'
HOST = '127.0.0.1'
PORT = 18_790
DEFAULT_BUS = 'I2C0'
FAULTS = ('nack', 'arbitration-loss', 'timeout', 'bus-error')
# `claimed` is derived: whether any function holds the pin.
PIN_FIELDS = (
    'mode',
    'level',
    'output',
    'pull',
    'drive',
    'function',
    'driven',
    'claimed',
)


class VirtualIoError(RuntimeError):
    """The manager is unreachable or rejected a request."""


class VirtualIoSpecError(ValueError):
    """A scenario's `[virtual_io]` table is malformed."""


def address_value(port: int = PORT) -> str:
    """Value of `BARRACUDA_VIRTUAL_IO_ADDR` for a System run."""

    return f'{HOST}:{port}'


class VirtualIoClient:
    """One connection to the virtual peripherals manager."""

    def __init__(self, port: int = PORT, timeout: float = 10.0):
        deadline = time.monotonic() + timeout
        while True:
            try:
                self._socket = socket.create_connection((HOST, port), timeout=5)
                break
            except OSError as exc:
                if time.monotonic() >= deadline:
                    raise VirtualIoError(
                        f'virtual peripherals manager at {HOST}:{port}: {exc}'
                    ) from exc
                time.sleep(0.1)
        self._file = self._socket.makefile('rw', encoding='utf-8', newline='\n')

    def close(self) -> None:
        self._file.close()
        self._socket.close()

    def __enter__(self) -> VirtualIoClient:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def request(self, op: str, **fields: Any) -> dict[str, Any]:
        """Send one request and return its successful response."""

        message = {'op': op, **{k: v for k, v in fields.items() if v is not None}}
        try:
            self._file.write(json.dumps(message) + '\n')
            self._file.flush()
            line = self._file.readline()
        except OSError as exc:
            raise VirtualIoError(f'{op}: {exc}') from exc
        if not line:
            raise VirtualIoError(f'{op}: the manager closed the connection')
        response = json.loads(line)
        if not response.get('ok'):
            raise VirtualIoError(f'{op}: {response.get("error")}')
        return response

    def pins(self) -> list[dict[str, Any]]:
        return self.request('pins')['pins']

    def pin(self, pin: str) -> dict[str, Any]:
        return self.request('pin', pin=pin)['pin']

    def set_input(self, pin: str, level: bool | None) -> dict[str, Any]:
        return self.request('set_input', pin=pin, level=level)['pin']

    def add_device(
        self,
        address: int,
        bus: str = DEFAULT_BUS,
        model: str = 'registers',
        data: str | None = None,
    ) -> None:
        self.request('add_device', bus=bus, address=address, model=model, data=data)

    def read_registers(
        self, address: int, length: int, offset: int = 0, bus: str = DEFAULT_BUS
    ) -> str:
        return self.request(
            'read_registers', bus=bus, address=address, offset=offset, length=length
        )['data']

    def inject_fault(
        self,
        fault: str,
        address: int | None = None,
        bus: str = DEFAULT_BUS,
        once: bool = True,
    ) -> int:
        return self.request(
            'inject_fault', bus=bus, address=address, fault=fault, once=once
        )['id']

    def events(self, since: int = 0) -> list[dict[str, Any]]:
        return self.request('events', since=since)['events']

    def violations(self) -> list[dict[str, Any]]:
        return self.request('violations')['violations']


@dataclass(frozen=True)
class Device:
    address: int
    bus: str = DEFAULT_BUS
    model: str = 'registers'
    data: str | None = None


@dataclass(frozen=True)
class Fault:
    fault: str
    address: int | None = None
    bus: str = DEFAULT_BUS
    once: bool = True


@dataclass(frozen=True)
class PinExpectation:
    pin: str
    fields: dict[str, Any]


@dataclass(frozen=True)
class RegisterExpectation:
    address: int
    data: str
    offset: int = 0
    bus: str = DEFAULT_BUS


@dataclass(frozen=True)
class VirtualIoSpec:
    """Virtual hardware set up before the chat and checked after it."""

    inputs: dict[str, bool] = field(default_factory=dict)
    devices: tuple[Device, ...] = ()
    faults: tuple[Fault, ...] = ()
    expect_pins: tuple[PinExpectation, ...] = ()
    expect_registers: tuple[RegisterExpectation, ...] = ()
    # Each entry must match at least one recorded event on every given field.
    expect_events: tuple[dict[str, Any], ...] = ()
    allow_violations: bool = False


def parse_spec(document: Any, path: Path) -> VirtualIoSpec:
    """Parse a scenario's `[virtual_io]` table."""

    if not isinstance(document, dict):
        raise VirtualIoSpecError(f'{path}: virtual_io must be a table')
    known = {
        'inputs',
        'devices',
        'faults',
        'expect_pins',
        'expect_registers',
        'expect_events',
        'allow_violations',
    }
    unknown = set(document) - known
    if unknown:
        raise VirtualIoSpecError(f'{path}: unknown virtual_io keys {sorted(unknown)}')
    inputs = document.get('inputs', {})
    if not isinstance(inputs, dict) or not all(
        isinstance(level, bool) for level in inputs.values()
    ):
        raise VirtualIoSpecError(f'{path}: virtual_io.inputs maps pins to true/false')
    return VirtualIoSpec(
        inputs=dict(inputs),
        devices=tuple(
            Device(
                address=_address(entry, path),
                bus=str(entry.get('bus', DEFAULT_BUS)),
                model=str(entry.get('model', 'registers')),
                data=_hex(entry.get('data'), path),
            )
            for entry in _tables(document, 'devices', path)
        ),
        faults=tuple(
            _fault(entry, path) for entry in _tables(document, 'faults', path)
        ),
        expect_pins=tuple(
            _pin_expectation(entry, path)
            for entry in _tables(document, 'expect_pins', path)
        ),
        expect_registers=tuple(
            RegisterExpectation(
                address=_address(entry, path),
                data=_required_hex(entry, path),
                offset=int(entry.get('offset', 0)),
                bus=str(entry.get('bus', DEFAULT_BUS)),
            )
            for entry in _tables(document, 'expect_registers', path)
        ),
        expect_events=tuple(
            dict(entry) for entry in _tables(document, 'expect_events', path)
        ),
        allow_violations=bool(document.get('allow_violations', False)),
    )


def apply_setup(client: VirtualIoClient, spec: VirtualIoSpec) -> None:
    """Drive inputs, attach devices, and inject faults before the chat."""

    for pin, level in spec.inputs.items():
        client.set_input(pin, level)
    for device in spec.devices:
        client.add_device(device.address, device.bus, device.model, device.data)
    for fault in spec.faults:
        client.inject_fault(fault.fault, fault.address, fault.bus, fault.once)


def check(client: VirtualIoClient, spec: VirtualIoSpec | None) -> list[str]:
    """Return failures for the expectations and any datasheet violation."""

    failures: list[str] = []
    if spec is not None:
        for expectation in spec.expect_pins:
            failures += pin_failures(client.pin(expectation.pin), expectation)
        for expectation in spec.expect_registers:
            length = len(expectation.data) // 2
            actual = client.read_registers(
                expectation.address, length, expectation.offset, expectation.bus
            )
            if actual != expectation.data.lower():
                failures.append(
                    f'virtual {expectation.bus}/{expectation.address:#04x} registers '
                    f'at {expectation.offset:#04x}: {actual} != {expectation.data.lower()}'
                )
        if spec.expect_events:
            events = client.events()
            failures += [
                f'no virtual I/O event matches {wanted}'
                for wanted in spec.expect_events
                if not any(event_matches(event, wanted) for event in events)
            ]
    if spec is None or not spec.allow_violations:
        failures += [
            f'virtual {v["model"]} at {v["bus"]}/{v["address"]:#04x} broke '
            f'{v["rule"]}: {v["message"]}'
            for v in client.violations()
        ]
    return failures


def pin_failures(actual: dict[str, Any], expectation: PinExpectation) -> list[str]:
    """Differences between a pin snapshot and the expected fields."""

    actual = {**actual, 'claimed': actual.get('function') is not None}
    return [
        f'virtual pin {expectation.pin} {name}: {actual.get(name)!r} != {wanted!r}'
        for name, wanted in expectation.fields.items()
        if actual.get(name) != wanted
    ]


def event_matches(event: dict[str, Any], wanted: dict[str, Any]) -> bool:
    """Whether `event` has every field of `wanted`; lists match by prefix."""

    for name, value in wanted.items():
        actual = event.get(name)
        if isinstance(value, list):
            if not isinstance(actual, list) or actual[: len(value)] != value:
                return False
        elif actual != value:
            return False
    return True


def snapshot(client: VirtualIoClient) -> dict[str, Any]:
    """Pins, buses, events, and violations for the scenario artifacts."""

    return {
        'pins': client.pins(),
        'buses': client.request('buses')['buses'],
        'events': client.events(),
        'violations': client.violations(),
    }


def _tables(document: dict[str, Any], key: str, path: Path) -> list[dict[str, Any]]:
    value = document.get(key, [])
    if not isinstance(value, list) or not all(isinstance(e, dict) for e in value):
        raise VirtualIoSpecError(f'{path}: virtual_io.{key} must be an array of tables')
    return value


def _address(entry: dict[str, Any], path: Path) -> int:
    address = entry.get('address')
    if (
        not isinstance(address, int)
        or isinstance(address, bool)
        or not 0 <= address <= 0x7F
    ):
        raise VirtualIoSpecError(f'{path}: virtual_io address must be a 7-bit integer')
    return address


def _hex(value: Any, path: Path) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str) or len(value) % 2:
        raise VirtualIoSpecError(f'{path}: virtual_io data must be an even hex string')
    try:
        bytes.fromhex(value)
    except ValueError as exc:
        raise VirtualIoSpecError(
            f'{path}: virtual_io data is not hex: {value!r}'
        ) from exc
    return value.lower()


def _required_hex(entry: dict[str, Any], path: Path) -> str:
    value = _hex(entry.get('data'), path)
    if not value:
        raise VirtualIoSpecError(f'{path}: virtual_io.expect_registers needs data')
    return value


def _fault(entry: dict[str, Any], path: Path) -> Fault:
    fault = entry.get('fault')
    if fault not in FAULTS:
        raise VirtualIoSpecError(f'{path}: virtual_io fault must be one of {FAULTS}')
    address = entry.get('address')
    return Fault(
        fault=fault,
        address=None if address is None else _address(entry, path),
        bus=str(entry.get('bus', DEFAULT_BUS)),
        once=bool(entry.get('once', True)),
    )


def _pin_expectation(entry: dict[str, Any], path: Path) -> PinExpectation:
    pin = entry.get('pin')
    if not isinstance(pin, str) or not pin:
        raise VirtualIoSpecError(f'{path}: virtual_io.expect_pins needs a pin name')
    fields = {name: value for name, value in entry.items() if name != 'pin'}
    unknown = set(fields) - set(PIN_FIELDS)
    if unknown or not fields:
        raise VirtualIoSpecError(
            f'{path}: virtual_io.expect_pins checks some of {PIN_FIELDS}'
        )
    return PinExpectation(pin=pin, fields=fields)
