"""Assertions over CLI transcripts, System logs, and tape replay counters."""

from __future__ import annotations

import re
from collections.abc import Sequence
from pathlib import Path

from .scenario import Scenario

# Tool results that mean the call never reached a working implementation.
# Records render as `<name><arguments JSON><result JSON>`, so a failed call has
# a result object that starts with an `error` key.
TOOL_FAILURE_PATTERNS = (r'tool not found', r'\binvalid arguments\b', r'\}\{"error":')


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
