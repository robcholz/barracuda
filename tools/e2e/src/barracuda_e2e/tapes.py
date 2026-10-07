"""Synthesize llm-tape replay tapes from scripted model responses."""

from __future__ import annotations

import base64
import json
import re
from collections.abc import Iterable, Iterator
from datetime import UTC, datetime, timedelta
from http import HTTPStatus
from pathlib import Path
from typing import Any

from .scenario import ModelResponse

SCRIPTED_API_PATH = '/v1'
CHAT_PATH = '/chat/completions'
# Offset between synthesized chunks; replay schedules them at these times.
CHUNK_INTERVAL_US = 2_000
NOW_PLACEHOLDER = re.compile(r'\$\{now(?:\+(\d+)s)?\}')


def expand_placeholders(value: Any, now: datetime) -> Any:
    """Replace `${now}` / `${now+Ns}` in strings with RFC3339 UTC milliseconds."""

    if isinstance(value, str):
        return NOW_PLACEHOLDER.sub(
            lambda match: _rfc3339(now + timedelta(seconds=int(match.group(1) or 0))),
            value,
        )
    if isinstance(value, dict):
        return {key: expand_placeholders(item, now) for key, item in value.items()}
    if isinstance(value, list):
        return [expand_placeholders(item, now) for item in value]
    return value


def _rfc3339(moment: datetime) -> str:
    return moment.astimezone(UTC).strftime('%Y-%m-%dT%H:%M:%S.') + (
        f'{moment.microsecond // 1000:03d}Z'
    )


def write_scripted_tape(responses: Iterable[ModelResponse], path: Path) -> int:
    """Write one OpenAI-compatible streaming interaction per response.

    Returns the number of interactions written.
    """

    events: list[dict[str, Any]] = [
        {
            'kind': 'tape_start',
            'version': 1,
            'created_at': datetime.now(UTC).isoformat(),
        }
    ]
    count = 0
    now = datetime.now(UTC)
    for index, response in enumerate(responses):
        events.extend(_interaction(index, response, now))
        count += 1
    events.append({'kind': 'tape_end'})
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        ''.join(json.dumps(event, ensure_ascii=False) + '\n' for event in events),
        encoding='utf-8',
    )
    return count


def sse_frames(response: ModelResponse, now: datetime | None = None) -> Iterator[str]:
    """Yield the OpenAI chat-completions SSE frames for one response."""

    now = now or datetime.now(UTC)
    if response.reasoning:
        yield _frame({'reasoning_content': response.reasoning})
    for piece in _split(response.text):
        yield _frame({'content': piece})
    for index, call in enumerate(response.tool_calls):
        yield _frame(
            {
                'tool_calls': [
                    {
                        'index': index,
                        'id': f'call_{index}',
                        'type': 'function',
                        'function': {
                            'name': call.name,
                            'arguments': json.dumps(
                                expand_placeholders(call.arguments, now),
                                ensure_ascii=False,
                            ),
                        },
                    }
                ]
            }
        )
    finish = 'tool_calls' if response.tool_calls else 'stop'
    yield _data({'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]})
    yield _data(
        {
            'choices': [],
            'usage': {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2},
        }
    )
    yield 'data: [DONE]\n\n'


def _interaction(
    index: int, response: ModelResponse, now: datetime
) -> Iterator[dict[str, Any]]:
    interaction_id = f'call-{index:06d}'
    path = SCRIPTED_API_PATH + CHAT_PATH
    yield {
        'kind': 'request',
        'interaction_id': interaction_id,
        'call_index': index,
        'method': 'POST',
        'path': path,
        'path_qs': path,
        'headers': [['content-type', 'application/json']],
        'body_sha256': '',
        'body_size': 0,
    }
    if response.raw is None:
        frames: list[str] = list(sse_frames(response, now))
        content_type = 'text/event-stream'
    else:
        frames = [response.raw]
        content_type = (
            'text/event-stream'
            if response.raw.startswith(('data:', ':', 'event:'))
            else 'application/json'
        )
    yield {
        'kind': 'response_start',
        'interaction_id': interaction_id,
        'at_us': CHUNK_INTERVAL_US,
        'status': response.status,
        'reason': _reason(response.status),
        'headers': [['content-type', content_type]],
    }
    at_us = CHUNK_INTERVAL_US
    seq = 0
    for seq, frame in enumerate(frames):
        at_us += CHUNK_INTERVAL_US
        yield {
            'kind': 'response_chunk',
            'interaction_id': interaction_id,
            'seq': seq,
            'at_us': at_us,
            'data_b64': base64.b64encode(frame.encode('utf-8')).decode('ascii'),
        }
    yield {
        'kind': 'response_end',
        'interaction_id': interaction_id,
        'at_us': at_us + CHUNK_INTERVAL_US,
        'outcome': 'upstream_error' if response.abort else 'eof',
    }


def _reason(status: int) -> str:
    try:
        return HTTPStatus(status).phrase
    except ValueError:
        return 'Unknown'


def _frame(delta: dict[str, Any]) -> str:
    return _data({'choices': [{'index': 0, 'delta': delta}]})


def _data(payload: dict[str, Any]) -> str:
    return f'data: {json.dumps(payload, ensure_ascii=False)}\n\n'


def _split(text: str, size: int = 16) -> list[str]:
    return [text[start : start + size] for start in range(0, len(text), size)]
