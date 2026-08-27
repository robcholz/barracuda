"""Append-only tape writer and strict tape loader."""

from __future__ import annotations

import asyncio
import base64
import binascii
import json
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, TextIO

from .headers import HeaderPairs

FORMAT_VERSION = 1


class TapeFormatError(ValueError):
    """Raised when a tape is incomplete or structurally invalid."""


@dataclass(frozen=True)
class RecordedRequest:
    """Metadata used to validate the next replay request."""

    interaction_id: str
    call_index: int
    method: str
    path: str
    path_qs: str
    headers: HeaderPairs
    body_sha256: str
    body_size: int


@dataclass(frozen=True)
class RecordedResponseStart:
    """The recorded response head and its offset from request start."""

    at_us: int
    status: int
    reason: str | None
    headers: HeaderPairs


@dataclass(frozen=True)
class RecordedChunk:
    """An uninterpreted response-body byte chunk."""

    seq: int
    at_us: int
    data: bytes


@dataclass(frozen=True)
class RecordedResponseEnd:
    """How and when the recorded response stream ended."""

    at_us: int
    outcome: str


@dataclass(frozen=True)
class Interaction:
    """One request and the byte stream returned for it."""

    request: RecordedRequest
    response_start: RecordedResponseStart
    chunks: tuple[RecordedChunk, ...]
    response_end: RecordedResponseEnd


@dataclass(frozen=True)
class Tape:
    """A validated sequence of recorded HTTP interactions."""

    version: int
    created_at: str
    interactions: tuple[Interaction, ...]


@dataclass
class _MutableInteraction:
    request: RecordedRequest
    response_start: RecordedResponseStart | None = None
    chunks: list[RecordedChunk] = field(default_factory=list)
    response_end: RecordedResponseEnd | None = None


class TapeWriter:
    """Append recording events to a new JSONL tape.

    Each event is flushed before control returns so a process crash leaves the
    longest possible valid prefix on disk. Concurrent HTTP handlers serialize
    only the small JSONL append operation.
    """

    def __init__(self, path: Path | str):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._file: TextIO = self.path.open('x', encoding='utf-8')
        self._lock = asyncio.Lock()
        self._next_call_index = 0
        self._closed = False
        self._write_line(
            {
                'kind': 'tape_start',
                'version': FORMAT_VERSION,
                'created_at': datetime.now(timezone.utc).isoformat(),
            }
        )

    async def request(
        self,
        *,
        method: str,
        path: str,
        path_qs: str,
        headers: HeaderPairs,
        body_sha256: str,
        body_size: int,
    ) -> tuple[str, int]:
        """Allocate an interaction and append its request metadata."""

        async with self._lock:
            self._ensure_open()
            call_index = self._next_call_index
            self._next_call_index += 1
            interaction_id = f'call-{call_index:06d}'
            self._write_line(
                {
                    'kind': 'request',
                    'interaction_id': interaction_id,
                    'call_index': call_index,
                    'method': method,
                    'path': path,
                    'path_qs': path_qs,
                    'headers': headers,
                    'body_sha256': body_sha256,
                    'body_size': body_size,
                }
            )
            return interaction_id, call_index

    async def response_start(
        self,
        interaction_id: str,
        *,
        at_us: int,
        status: int,
        reason: str | None,
        headers: HeaderPairs,
    ) -> None:
        """Append a response head event."""

        await self._append(
            {
                'kind': 'response_start',
                'interaction_id': interaction_id,
                'at_us': at_us,
                'status': status,
                'reason': reason,
                'headers': headers,
            }
        )

    async def chunk(
        self,
        interaction_id: str,
        *,
        seq: int,
        at_us: int,
        data: bytes,
    ) -> None:
        """Append one uninterpreted response-body chunk."""

        await self._append(
            {
                'kind': 'response_chunk',
                'interaction_id': interaction_id,
                'seq': seq,
                'at_us': at_us,
                'data_b64': base64.b64encode(data).decode('ascii'),
            }
        )

    async def response_end(
        self,
        interaction_id: str,
        *,
        at_us: int,
        outcome: str,
    ) -> None:
        """Append the terminal stream event."""

        await self._append(
            {
                'kind': 'response_end',
                'interaction_id': interaction_id,
                'at_us': at_us,
                'outcome': outcome,
            }
        )

    async def close(self) -> None:
        """Finish and close the tape. Repeated calls are harmless."""

        async with self._lock:
            if self._closed:
                return
            self._write_line({'kind': 'tape_end'})
            self._file.close()
            self._closed = True

    async def _append(self, event: dict[str, Any]) -> None:
        async with self._lock:
            self._ensure_open()
            self._write_line(event)

    def _write_line(self, event: dict[str, Any]) -> None:
        self._file.write(
            json.dumps(event, separators=(',', ':'), ensure_ascii=False) + '\n'
        )
        self._file.flush()

    def _ensure_open(self) -> None:
        if self._closed:
            raise RuntimeError('tape writer is closed')


def load_tape(path: Path | str) -> Tape:
    """Load and fully validate an original-timing replay tape."""

    tape_path = Path(path)
    start: dict[str, Any] | None = None
    saw_tape_end = False
    mutable: dict[str, _MutableInteraction] = {}

    try:
        lines = tape_path.read_text(encoding='utf-8').splitlines()
    except OSError as exc:
        raise TapeFormatError(f'cannot read tape {tape_path}: {exc}') from exc

    for line_number, line in enumerate(lines, start=1):
        if not line.strip():
            continue
        event = _parse_line(line, line_number)
        kind = _required_str(event, 'kind', line_number)

        if kind == 'tape_start':
            if start is not None or mutable:
                raise TapeFormatError(f'line {line_number}: duplicate tape_start')
            start = event
        elif kind == 'request':
            interaction_id = _required_str(event, 'interaction_id', line_number)
            if interaction_id in mutable:
                raise TapeFormatError(
                    f'line {line_number}: duplicate interaction {interaction_id}'
                )
            mutable[interaction_id] = _MutableInteraction(
                request=_parse_request(event, line_number)
            )
        elif kind == 'response_start':
            item = _find_interaction(mutable, event, line_number)
            if item.response_start is not None:
                raise TapeFormatError(f'line {line_number}: duplicate response_start')
            item.response_start = _parse_response_start(event, line_number)
        elif kind == 'response_chunk':
            item = _find_interaction(mutable, event, line_number)
            if item.response_start is None:
                raise TapeFormatError(
                    f'line {line_number}: chunk precedes response_start'
                )
            item.chunks.append(_parse_chunk(event, line_number))
        elif kind == 'response_end':
            item = _find_interaction(mutable, event, line_number)
            if item.response_end is not None:
                raise TapeFormatError(f'line {line_number}: duplicate response_end')
            item.response_end = _parse_response_end(event, line_number)
        elif kind == 'tape_end':
            saw_tape_end = True
        else:
            raise TapeFormatError(f'line {line_number}: unknown event kind {kind!r}')

    if start is None:
        raise TapeFormatError('missing tape_start')
    version = _required_int(start, 'version', 1)
    if version != FORMAT_VERSION:
        raise TapeFormatError(
            f'unsupported tape version {version}; expected {FORMAT_VERSION}'
        )
    if not saw_tape_end:
        raise TapeFormatError('tape is incomplete: missing tape_end')

    created_at = _required_str(start, 'created_at', 1)
    interactions = _finalize_interactions(mutable)
    return Tape(
        version=version,
        created_at=created_at,
        interactions=tuple(interactions),
    )


def _parse_line(line: str, line_number: int) -> dict[str, Any]:
    try:
        event = json.loads(line)
    except json.JSONDecodeError as exc:
        raise TapeFormatError(f'line {line_number}: invalid JSON: {exc.msg}') from exc
    if not isinstance(event, dict):
        raise TapeFormatError(f'line {line_number}: event must be an object')
    return event


def _parse_request(event: dict[str, Any], line_number: int) -> RecordedRequest:
    return RecordedRequest(
        interaction_id=_required_str(event, 'interaction_id', line_number),
        call_index=_non_negative_int(event, 'call_index', line_number),
        method=_required_str(event, 'method', line_number),
        path=_required_str(event, 'path', line_number),
        path_qs=_required_str(event, 'path_qs', line_number),
        headers=_parse_headers(event.get('headers'), line_number),
        body_sha256=_required_str(event, 'body_sha256', line_number),
        body_size=_non_negative_int(event, 'body_size', line_number),
    )


def _parse_response_start(
    event: dict[str, Any], line_number: int
) -> RecordedResponseStart:
    reason = event.get('reason')
    if reason is not None and not isinstance(reason, str):
        raise TapeFormatError(f'line {line_number}: reason must be a string or null')
    status = _required_int(event, 'status', line_number)
    if not 100 <= status <= 599:
        raise TapeFormatError(f'line {line_number}: invalid HTTP status {status}')
    return RecordedResponseStart(
        at_us=_non_negative_int(event, 'at_us', line_number),
        status=status,
        reason=reason,
        headers=_parse_headers(event.get('headers'), line_number),
    )


def _parse_chunk(event: dict[str, Any], line_number: int) -> RecordedChunk:
    encoded = _required_str(event, 'data_b64', line_number)
    try:
        data = base64.b64decode(encoded, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise TapeFormatError(f'line {line_number}: invalid chunk base64') from exc
    return RecordedChunk(
        seq=_non_negative_int(event, 'seq', line_number),
        at_us=_non_negative_int(event, 'at_us', line_number),
        data=data,
    )


def _parse_response_end(event: dict[str, Any], line_number: int) -> RecordedResponseEnd:
    return RecordedResponseEnd(
        at_us=_non_negative_int(event, 'at_us', line_number),
        outcome=_required_str(event, 'outcome', line_number),
    )


def _find_interaction(
    mutable: dict[str, _MutableInteraction],
    event: dict[str, Any],
    line_number: int,
) -> _MutableInteraction:
    interaction_id = _required_str(event, 'interaction_id', line_number)
    try:
        return mutable[interaction_id]
    except KeyError as exc:
        raise TapeFormatError(
            f'line {line_number}: unknown interaction {interaction_id}'
        ) from exc


def _finalize_interactions(
    mutable: dict[str, _MutableInteraction],
) -> list[Interaction]:
    items = sorted(mutable.values(), key=lambda item: item.request.call_index)
    expected_indices = list(range(len(items)))
    actual_indices = [item.request.call_index for item in items]
    if actual_indices != expected_indices:
        raise TapeFormatError(
            f'call_index values must be contiguous from zero; got {actual_indices}'
        )

    interactions: list[Interaction] = []
    for item in items:
        interaction_id = item.request.interaction_id
        if item.response_start is None:
            raise TapeFormatError(f'{interaction_id}: missing response_start')
        if item.response_end is None:
            raise TapeFormatError(f'{interaction_id}: missing response_end')
        expected_seq = list(range(len(item.chunks)))
        actual_seq = [chunk.seq for chunk in item.chunks]
        if actual_seq != expected_seq:
            raise TapeFormatError(
                f'{interaction_id}: chunk seq must be contiguous; got {actual_seq}'
            )
        timeline = [item.response_start.at_us]
        timeline.extend(chunk.at_us for chunk in item.chunks)
        timeline.append(item.response_end.at_us)
        if timeline != sorted(timeline):
            raise TapeFormatError(
                f'{interaction_id}: response event times must be non-decreasing'
            )
        interactions.append(
            Interaction(
                request=item.request,
                response_start=item.response_start,
                chunks=tuple(item.chunks),
                response_end=item.response_end,
            )
        )
    return interactions


def _parse_headers(value: Any, line_number: int) -> HeaderPairs:
    if not isinstance(value, list):
        raise TapeFormatError(f'line {line_number}: headers must be a list')
    headers: HeaderPairs = []
    for pair in value:
        if (
            not isinstance(pair, list)
            or len(pair) != 2
            or not all(isinstance(part, str) for part in pair)
        ):
            raise TapeFormatError(
                f'line {line_number}: each header must be a string pair'
            )
        headers.append((pair[0], pair[1]))
    return headers


def _required_str(event: dict[str, Any], key: str, line_number: int) -> str:
    value = event.get(key)
    if not isinstance(value, str):
        raise TapeFormatError(f'line {line_number}: {key} must be a string')
    return value


def _required_int(event: dict[str, Any], key: str, line_number: int) -> int:
    value = event.get(key)
    if not isinstance(value, int) or isinstance(value, bool):
        raise TapeFormatError(f'line {line_number}: {key} must be an integer')
    return value


def _non_negative_int(event: dict[str, Any], key: str, line_number: int) -> int:
    value = _required_int(event, key, line_number)
    if value < 0:
        raise TapeFormatError(f'line {line_number}: {key} must be non-negative')
    return value
