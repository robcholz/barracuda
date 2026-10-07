from __future__ import annotations

import base64
import json
import struct
from datetime import UTC, datetime
from pathlib import Path

import pytest
from llm_tape.tape import load_tape

from barracuda_e2e.cli import build_parser, shard
from barracuda_e2e.assertions import (
    check_step_requests,
    check_heap,
    check_logs,
    check_replay,
    check_transcript,
    heap_high_water,
)
from barracuda_e2e.ntp import NTP_UNIX_OFFSET, sntp_reply
from barracuda_e2e.scenario import (
    ModelResponse,
    ScenarioError,
    ToolCall,
    discover,
    load_scenario,
)
from barracuda_e2e.tapes import expand_placeholders, write_scripted_tape

SCENARIOS = Path(__file__).resolve().parents[1] / 'scenarios'


def test_every_bundled_scenario_parses():
    paths = discover(SCENARIOS)
    assert paths
    for path in paths:
        load_scenario(path)


def test_scripted_tape_round_trips_through_llm_tape(tmp_path):
    tape = tmp_path / 'scripted.jsonl'
    responses = [
        ModelResponse(tool_calls=(ToolCall('time_now', {}),)),
        ModelResponse(text='done', reasoning='thinking'),
    ]
    assert write_scripted_tape(responses, tape) == 2

    loaded = load_tape(tape)
    assert [i.request.path for i in loaded.interactions] == [
        '/v1/chat/completions',
        '/v1/chat/completions',
    ]
    first = b''.join(chunk.data for chunk in loaded.interactions[0].chunks).decode()
    assert '"name": "time_now"' in first
    assert '"finish_reason": "tool_calls"' in first
    assert first.endswith('data: [DONE]\n\n')
    second = b''.join(chunk.data for chunk in loaded.interactions[1].chunks).decode()
    assert '"reasoning_content": "thinking"' in second
    assert '"content": "done"' in second


def test_scripted_faults_set_status_body_and_abort(tmp_path):
    tape = tmp_path / 'faults.jsonl'
    responses = [
        ModelResponse(status=503, raw='{"error": "busy"}'),
        ModelResponse(raw='data: {"choices": []}\n\n', abort=True),
    ]
    assert write_scripted_tape(responses, tape) == 2

    first, second = load_tape(tape).interactions
    assert first.response_start.status == 503
    assert ('content-type', 'application/json') in first.response_start.headers
    assert b''.join(chunk.data for chunk in first.chunks) == b'{"error": "busy"}'
    assert first.response_end.outcome == 'eof'
    assert ('content-type', 'text/event-stream') in second.response_start.headers
    assert second.response_end.outcome == 'upstream_error'


def test_raw_responses_exclude_text_and_tool_calls(tmp_path):
    path = tmp_path / 'raw.toml'
    path.write_text(
        'name = "x"\n[model]\nmode = "scripted"\n[[model.responses]]\n'
        'raw = "data: x"\ntext = "y"\n[[steps]]\nsend = "hi"\n',
        encoding='utf-8',
    )
    with pytest.raises(ScenarioError):
        load_scenario(path)


def test_now_placeholders_expand_to_rfc3339_milliseconds():
    now = datetime(2026, 1, 2, 3, 4, 5, 678_900, tzinfo=UTC)
    expanded = expand_placeholders({'at': '${now+8s}', 'list': ['${now}'], 'n': 1}, now)
    assert expanded == {
        'at': '2026-01-02T03:04:13.678Z',
        'list': ['2026-01-02T03:04:05.678Z'],
        'n': 1,
    }


def test_recorded_step_requests_are_checked_within_each_step(tmp_path):
    scenario_path = tmp_path / 'scenarios' / 'demo.toml'
    scenario_path.parent.mkdir()
    scenario_path.write_text(
        'name = "demo"\n[model]\nmode = "recorded"\n'
        '[[steps]]\nsend = "first \\"step\\""\nrequest_contains = ["alpha", "beta"]\n'
        '[[steps]]\nsend = "second"\nrequest_contains = ["gamma"]\n',
        encoding='utf-8',
    )
    scenario = load_scenario(scenario_path)
    requests = tmp_path / 'requests'
    requests.mkdir()
    for index, body in enumerate(
        [
            '{"content":"first \\"step\\"","x":"alpha"}',
            '{"goal":"beta"}',
            '{"content":"first \\"step\\"","y":"second","z":"delta"}',
        ]
    ):
        (requests / f'call-{index:06d}.body').write_text(body, encoding='utf-8')
    assert check_step_requests(scenario, requests) == [
        "step 1: no model request contains 'gamma'"
    ]


def test_step_request_checks_need_a_recorded_scenario(tmp_path):
    path = tmp_path / 'scripted.toml'
    path.write_text(
        'name = "x"\n[model]\nmode = "scripted"\n[[model.responses]]\ntext = "y"\n'
        '[[steps]]\nsend = "hi"\nrequest_contains = ["z"]\n',
        encoding='utf-8',
    )
    with pytest.raises(ScenarioError):
        load_scenario(path)


def test_transcript_checks_replies_tools_and_failures(tmp_path):
    scenario_path = tmp_path / 'scenarios' / 'demo.toml'
    scenario_path.parent.mkdir()
    scenario_path.write_text(
        'name = "demo"\n[model]\nmode = "scripted"\n'
        '[[model.responses]]\ntext = "ok"\n'
        '[[steps]]\nsend = "hi"\nreply_contains = ["ok"]\ntool_contains = ["42"]\n',
        encoding='utf-8',
    )
    scenario = load_scenario(scenario_path)
    passing = [
        {'turn': 0, 'kind': 'tool', 'text': 'answer{}{"value":42}'},
        {'turn': 0, 'kind': 'reply', 'text': 'ok'},
    ]
    assert check_transcript(scenario, passing) == []

    failing = [
        {'turn': 0, 'kind': 'tool', 'text': 'answer{}{"error":"broken"}'},
        {'turn': 0, 'kind': 'reply', 'text': 'nope'},
    ]
    failures = check_transcript(scenario, failing)
    assert any('reply lacks' in failure for failure in failures)
    assert any('tool output lacks' in failure for failure in failures)
    assert any('tool failed' in failure for failure in failures)


def test_log_and_replay_checks(tmp_path):
    scenario_path = tmp_path / 'scenarios' / 'demo.toml'
    scenario_path.parent.mkdir()
    scenario_path.write_text(
        'name = "demo"\n[model]\nmode = "none"\n'
        '[[http]]\npath = "/"\n'
        '[logs]\nexpect = ["started"]\nallow = ["expected ERROR"]\n',
        encoding='utf-8',
    )
    scenario = load_scenario(scenario_path)
    assert check_logs(scenario, 'System started\nexpected ERROR here\n') == []
    assert check_logs(scenario, 'ERROR boom\n') == [
        'log lacks /started/',
        'forbidden log /\\bERROR\\b/: ERROR boom',
    ]
    assert check_replay({'request_completed': 2, 'request_rejected': 0}, 2) == []
    assert len(check_replay({'request_completed': 1, 'request_rejected': 1}, 2)) == 2


def test_invalid_scenarios_are_rejected(tmp_path):
    path = tmp_path / 'bad.toml'
    path.write_text('name = "bad"\n[model]\nmode = "live"\n', encoding='utf-8')
    with pytest.raises(ScenarioError):
        load_scenario(path)


def test_sntp_reply_echoes_the_client_transmit_time():
    request = bytearray(48)
    request[0] = 0x23  # version 4, client mode
    request[40:48] = b'\x01\x02\x03\x04\x05\x06\x07\x08'
    reply = sntp_reply(bytes(request), 1_700_000_000.5)

    assert len(reply) == 48
    assert reply[0] & 0b111 == 4  # server mode
    assert reply[24:32] == request[40:48]  # originate = client transmit
    seconds, fraction = struct.unpack('>II', reply[40:48])
    assert seconds == 1_700_000_000 + NTP_UNIX_OFFSET
    assert fraction == 1 << 31


def test_tool_call_arguments_are_json_encoded(tmp_path):
    tape = tmp_path / 'call.jsonl'
    write_scripted_tape(
        [ModelResponse(tool_calls=(ToolCall('vm_run', {'source': "print('é')"}),))],
        tape,
    )
    chunks = [
        json.loads(line)
        for line in tape.read_text(encoding='utf-8').splitlines()
        if '"response_chunk"' in line
    ]
    frames = ''.join(base64.b64decode(chunk['data_b64']).decode() for chunk in chunks)
    payload = json.loads(frames.split('data: ')[1])
    arguments = payload['choices'][0]['delta']['tool_calls'][0]['function']['arguments']
    assert json.loads(arguments) == {'source': "print('é')"}


def test_heap_high_water_is_the_largest_report_and_respects_the_budget(tmp_path):
    path = tmp_path / 'heap.toml'
    path.write_text(
        'name = "heap"\n[[http]]\npath = "/"\n[memory]\nheap_high_water_max = 9000\n',
        encoding='utf-8',
    )
    scenario = load_scenario(path)
    log = (
        'ordinary heap high-water: 8192 bytes (current 4000)\n'
        'ordinary heap high-water: 4096 bytes (current 100)\n'
    )
    assert heap_high_water(log) == 8192
    assert heap_high_water('no reports') is None
    assert check_heap(scenario, log) == []
    over = log + 'ordinary heap high-water: 12288 bytes (current 1)\n'
    assert check_heap(scenario, over) == [
        'ordinary heap high-water 12288 bytes exceeds 9000'
    ]
    assert check_heap(scenario, '') == [
        'System reported no ordinary-heap high-water mark'
    ]


def test_shards_partition_the_scenarios():
    scenarios = [load_scenario(path) for path in discover(SCENARIOS)]
    shards = [shard(scenarios, index, 4) for index in range(1, 5)]
    assert sorted(s.slug for part in shards for s in part) == sorted(
        s.slug for s in scenarios
    )
    assert max(map(len, shards)) - min(map(len, shards)) <= 1


@pytest.mark.parametrize('spec', ['0/4', '5/4', '2', 'a/b'])
def test_invalid_shards_are_rejected(spec):
    with pytest.raises(SystemExit):
        build_parser().parse_args(['run', '--shard', spec])


def test_coverage_groups_files_by_plugin_crate_and_skips_dependencies():
    from barracuda_e2e.coverage import component_of, summarize
    from barracuda_e2e.system import WORKSPACE

    session = str(WORKSPACE / 'plugins/agent/crates/session/src/lib.rs')
    assert component_of(session) == 'plugins/agent/session'
    assert component_of(str(WORKSPACE / 'shared/vfs/src/lib.rs')) == 'shared/vfs'
    assert component_of('/root/.cargo/registry/src/serde/lib.rs') is None

    def entry(path, covered, count):
        stats = {'covered': covered, 'count': count}
        return {
            'filename': str(WORKSPACE / path),
            'summary': {'lines': stats, 'functions': stats, 'regions': stats},
        }

    components = summarize(
        [
            entry('platforms/linux/src/a.rs', 1, 2),
            entry('shared/vfs/src/a.rs', 3, 4),
            entry('shared/vfs/src/b.rs', 1, 4),
        ]
    )
    assert [(c.name, c.business, c.lines) for c in components] == [
        ('shared/vfs', True, (4, 8)),
        ('platforms/linux', False, (1, 2)),
    ]
