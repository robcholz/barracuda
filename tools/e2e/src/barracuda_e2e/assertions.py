"""Assertions over CLI transcripts, System logs, and tape replay counters."""

from __future__ import annotations

import json
import re
from collections.abc import Sequence
from pathlib import Path

from .scenario import Scenario

# Tool results that mean the call never reached a working implementation.
# Records render as `<name><arguments JSON><result JSON>`, so a failed call has
# a result object that starts with an `error` key.
TOOL_FAILURE_PATTERNS = (r'tool not found', r'\binvalid arguments\b', r'\}\{"error":')
HEAP_HIGH_WATER = re.compile(r'ordinary heap high-water: (\d+) bytes')


def check_transcript(
    scenario: Scenario, records: Sequence[dict[str, object]]
) -> list[str]:
    """Return failures for per-step reply expectations."""

    failures: list[str] = []
    for turn, step in enumerate(scenario.steps):
        messages = [record for record in records if record.get('turn') == turn]
        reply = ''.join(
            str(record.get('text', ''))
            for record in messages
            if record.get('kind') == 'reply'
        )
        kinds = {str(record.get('kind')) for record in messages}
        tools = '\n'.join(
            str(record.get('text', ''))
            for record in messages
            if record.get('kind') == 'tool'
        )
        notices = '\n'.join(
            str(record.get('text', ''))
            for record in messages
            if record.get('kind') == 'notice'
        )
        if not messages:
            failures.append(f'step {turn}: no reply was received')
            continue
        for needle in step.reply_contains:
            if needle not in reply:
                failures.append(f'step {turn}: reply lacks {needle!r}: {reply!r}')
        for pattern in step.reply_matches:
            if re.search(pattern, reply) is None:
                failures.append(f'step {turn}: reply does not match /{pattern}/')
        for needle in step.tool_contains:
            if needle not in tools:
                failures.append(f'step {turn}: tool output lacks {needle!r}: {tools!r}')
        for needle in step.notice_contains:
            if needle not in notices:
                failures.append(f'step {turn}: notices lack {needle!r}: {notices!r}')
        if not step.tool_errors_allowed:
            for pattern in TOOL_FAILURE_PATTERNS:
                for line in tools.splitlines():
                    if re.search(pattern, line):
                        failures.append(f'step {turn}: tool failed: {line[:300]}')
        for kind in step.kinds:
            if kind not in kinds:
                failures.append(
                    f'step {turn}: no {kind!r} message (saw {sorted(kinds)})'
                )
    return failures


def check_logs(scenario: Scenario, log: str) -> list[str]:
    """Return failures for required and forbidden System log patterns."""

    failures = [
        f'log lacks /{pattern}/'
        for pattern in scenario.expect_logs
        if re.search(pattern, log) is None
    ]
    allowed = [re.compile(pattern) for pattern in scenario.allow_logs]
    for pattern in scenario.forbid_logs:
        for line in log.splitlines():
            if re.search(pattern, line) and not any(
                allow.search(line) for allow in allowed
            ):
                failures.append(f'forbidden log /{pattern}/: {line.strip()}')
    return failures


def heap_high_water(log: str) -> int | None:
    """Largest ordinary-heap high-water mark the System reported, if any."""

    peaks = [int(match.group(1)) for match in HEAP_HIGH_WATER.finditer(log)]
    return max(peaks, default=None)


def check_heap(scenario: Scenario, log: str) -> list[str]:
    """The reported ordinary-heap high-water mark stays within the budget."""

    if scenario.heap_high_water_max is None:
        return []
    peak = heap_high_water(log)
    if peak is None:
        return ['System reported no ordinary-heap high-water mark']
    if peak > scenario.heap_high_water_max:
        return [
            f'ordinary heap high-water {peak} bytes exceeds '
            f'{scenario.heap_high_water_max}'
        ]
    return []


def check_replay(counts: dict[str, int], interactions: int) -> list[str]:
    """Every recorded interaction is served exactly once and none is rejected."""

    failures: list[str] = []
    if counts.get('request_rejected', 0):
        failures.append(f'llm-tape rejected {counts["request_rejected"]} request(s)')
    completed = counts.get('request_completed', 0)
    if completed != interactions:
        failures.append(f'model calls: expected {interactions}, replayed {completed}')
    return failures


def check_requests(scenario: Scenario, requests: Path) -> list[str]:
    """Each scripted response's request contains the declared substrings."""

    failures: list[str] = []
    for index, response in enumerate(scenario.responses):
        if not response.request_contains:
            continue
        body_path = requests / f'call-{index:06d}.body'
        if not body_path.exists():
            failures.append(f'model call {index} was never made')
            continue
        body = body_path.read_text(encoding='utf-8', errors='replace')
        for needle in response.request_contains:
            if needle not in body:
                failures.append(f'model call {index} request lacks {needle!r}')
    return failures


def check_step_requests(scenario: Scenario, requests: Path) -> list[str]:
    """Each recorded step's model requests contain its declared substrings.

    A step's requests run from the first request whose body carries the step's
    message to the first one carrying the next step's message, so subagent and
    detached-turn requests made in between count for the step.
    """

    if not any(step.request_contains for step in scenario.steps):
        return []
    bodies = [
        path.read_text(encoding='utf-8', errors='replace')
        for path in sorted(requests.glob('call-*.body'))
    ]
    starts = [_first_request_with(bodies, step.send) for step in scenario.steps]
    failures: list[str] = []
    for turn, step in enumerate(scenario.steps):
        if not step.request_contains:
            continue
        start = starts[turn]
        if start is None:
            failures.append(f'step {turn}: no model request carried its message')
            continue
        later = [index for index in starts[turn + 1 :] if index is not None]
        window = bodies[start : min(later, default=len(bodies))]
        for needle in step.request_contains:
            if not any(needle in body for body in window):
                failures.append(f'step {turn}: no model request contains {needle!r}')
    return failures


def _first_request_with(bodies: Sequence[str], message: str) -> int | None:
    encoded = json.dumps(message, ensure_ascii=False)[1:-1]
    return next(
        (index for index, body in enumerate(bodies) if encoded in body), None
    )
