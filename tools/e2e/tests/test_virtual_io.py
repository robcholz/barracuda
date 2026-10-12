from __future__ import annotations

import json
import socket
import threading
from pathlib import Path

import pytest

from barracuda_e2e import virtual_io
from barracuda_e2e.scenario import ScenarioError, load_scenario


class FakeManager:
    """A loopback server answering the control protocol from canned state."""

    def __init__(self):
        self.requests: list[dict] = []
        self.violations: list[dict] = []
        self.pins = {
            'vio-0': {'name': 'vio-0', 'mode': 'output', 'level': True, 'output': True},
        }
        self.registers = '00a1b2'
        self.events = [
            {
                'seq': 1,
                'kind': 'i2c',
                'bus': 'I2C0',
                'address': 81,
                'result': 'nack',
                'operations': [{'write': '00'}, {'read': '11'}],
            },
        ]
        self._server = socket.create_server(('127.0.0.1', 0))
        self.port = self._server.getsockname()[1]
        threading.Thread(target=self._serve, daemon=True).start()

    def _serve(self) -> None:
        connection, _ = self._server.accept()
        with connection, connection.makefile('rw') as stream:
            for line in stream:
                request = json.loads(line)
                self.requests.append(request)
                stream.write(json.dumps(self._answer(request)) + '\n')
                stream.flush()

    def _answer(self, request: dict) -> dict:
        op = request['op']
        if op == 'pin':
            return {'ok': True, 'pin': self.pins[request['pin']]}
        if op == 'set_input':
            return {
                'ok': True,
                'pin': {'name': request['pin'], 'driven': request['level']},
            }
        if op == 'read_registers':
            start = request['offset'] * 2
            return {
                'ok': True,
                'data': self.registers[start : start + request['length'] * 2],
            }
        if op == 'events':
            return {'ok': True, 'events': self.events}
        if op == 'violations':
            return {'ok': True, 'violations': self.violations}
        if op == 'inject_fault':
            return {'ok': True, 'id': 1}
        if op == 'boom':
            return {'ok': False, 'error': 'unknown op'}
        return {'ok': True}


def write_scenario(tmp_path: Path, table: str) -> Path:
    path = tmp_path / 'vio.toml'
    path.write_text(
        'name = "vio"\n[model]\nmode = "none"\n[[http]]\npath = "/"\n' + table,
        encoding='utf-8',
    )
    return path


def test_virtual_io_tables_parse_into_a_spec(tmp_path):
    path = write_scenario(
        tmp_path,
        """
[virtual_io]
inputs = { vio-2 = true }

[[virtual_io.devices]]
address = 0x50
data = "0A0b"

[[virtual_io.faults]]
address = 0x51
fault = "timeout"
once = false

[[virtual_io.expect_pins]]
pin = "vio-0"
mode = "output"
level = true

[[virtual_io.expect_registers]]
address = 0x50
offset = 1
data = "0b"

[[virtual_io.expect_events]]
kind = "i2c"
result = "nack"
""",
    )
    spec = load_scenario(path).virtual_io
    assert spec is not None
    assert spec.inputs == {'vio-2': True}
    assert spec.devices == (virtual_io.Device(address=0x50, data='0a0b'),)
    assert spec.faults == (virtual_io.Fault('timeout', 0x51, 'I2C0', False),)
    assert spec.expect_pins[0].fields == {'mode': 'output', 'level': True}
    assert spec.expect_registers[0].offset == 1
    assert spec.expect_events == ({'kind': 'i2c', 'result': 'nack'},)
    assert load_scenario(write_scenario(tmp_path, '')).virtual_io is None


@pytest.mark.parametrize(
    'table',
    [
        '[virtual_io]\nsurprise = 1\n',
        '[virtual_io]\ninputs = { vio-0 = 1 }\n',
        '[[virtual_io.devices]]\naddress = 200\n',
        '[[virtual_io.devices]]\naddress = 1\ndata = "abc"\n',
        '[[virtual_io.faults]]\nfault = "gremlins"\n',
        '[[virtual_io.expect_pins]]\npin = "vio-0"\ncolour = "red"\n',
        '[[virtual_io.expect_registers]]\naddress = 1\n',
    ],
)
def test_malformed_virtual_io_tables_are_rejected(tmp_path, table):
    with pytest.raises(ScenarioError):
        load_scenario(write_scenario(tmp_path, table))


def test_pin_expectations_compare_fields_and_derive_claimed():
    pin = {'name': 'vio-6', 'mode': 'disabled', 'function': None}
    unclaimed = virtual_io.PinExpectation(
        'vio-6', {'claimed': False, 'mode': 'disabled'}
    )
    assert virtual_io.pin_failures(pin, unclaimed) == []
    claimed = virtual_io.PinExpectation('vio-6', {'claimed': True})
    assert virtual_io.pin_failures(pin, claimed) == [
        'virtual pin vio-6 claimed: False != True'
    ]


def test_event_matching_uses_fields_and_list_prefixes():
    event = {
        'kind': 'i2c',
        'address': 80,
        'operations': [{'write': '00'}, {'read': 'ff'}],
    }
    assert virtual_io.event_matches(event, {'kind': 'i2c'})
    assert virtual_io.event_matches(event, {'operations': [{'write': '00'}]})
    assert not virtual_io.event_matches(event, {'operations': [{'read': 'ff'}]})
    assert not virtual_io.event_matches(event, {'address': 81})


def test_setup_and_checks_talk_to_the_manager():
    manager = FakeManager()
    spec = virtual_io.VirtualIoSpec(
        inputs={'vio-3': False},
        devices=(virtual_io.Device(address=0x50, data='01'),),
        faults=(virtual_io.Fault('nack', address=0x51),),
        expect_pins=(
            virtual_io.PinExpectation('vio-0', {'level': False, 'mode': 'output'}),
        ),
        expect_registers=(
            virtual_io.RegisterExpectation(address=0x50, offset=1, data='a1b2'),
            virtual_io.RegisterExpectation(address=0x50, offset=1, data='ffff'),
        ),
        expect_events=({'result': 'nack'}, {'result': 'timeout'}),
    )
    manager.violations = [
        {
            'model': 'rtc',
            'bus': 'I2C0',
            'address': 0x32,
            'rule': 'tPOR',
            'message': 'early',
        }
    ]
    with virtual_io.VirtualIoClient(manager.port) as client:
        virtual_io.apply_setup(client, spec)
        failures = virtual_io.check(client, spec)
        with pytest.raises(virtual_io.VirtualIoError, match='unknown op'):
            client.request('boom')
    ops = [request['op'] for request in manager.requests]
    assert ops[:3] == ['set_input', 'add_device', 'inject_fault']
    assert manager.requests[0] == {'op': 'set_input', 'pin': 'vio-3', 'level': False}
    assert manager.requests[2]['once'] is True
    assert failures == [
        'virtual pin vio-0 level: True != False',
        'virtual I2C0/0x50 registers at 0x01: a1b2 != ffff',
        "no virtual I/O event matches {'result': 'timeout'}",
        'virtual rtc at I2C0/0x32 broke tPOR: early',
    ]


def test_violations_fail_scenarios_without_a_virtual_io_table():
    manager = FakeManager()
    manager.violations = [
        {'model': 'm', 'bus': 'I2C1', 'address': 1, 'rule': 'r', 'message': 'x'}
    ]
    with virtual_io.VirtualIoClient(manager.port) as client:
        assert virtual_io.check(client, None) == ['virtual m at I2C1/0x01 broke r: x']
        allowed = virtual_io.VirtualIoSpec(allow_violations=True)
        assert virtual_io.check(client, allowed) == []


def test_an_unreachable_manager_is_reported():
    with socket.create_server(('127.0.0.1', 0)) as probe:
        port = probe.getsockname()[1]
    with pytest.raises(virtual_io.VirtualIoError, match='virtual peripherals manager'):
        virtual_io.VirtualIoClient(port, timeout=0.3)


def test_the_system_address_is_loopback():
    assert virtual_io.address_value(1234) == '127.0.0.1:1234'
