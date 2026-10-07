"""Scenario files: conversation steps, model behaviour, and assertions."""

from __future__ import annotations

import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .virtual_io import VirtualIoSpec, VirtualIoSpecError, parse_spec

DEFAULT_FORBIDDEN_LOGS = (r'\bERROR\b', r'panicked at')


class ScenarioError(ValueError):
    """A scenario file is malformed."""


@dataclass(frozen=True)
class ToolCall:
    """One scripted model tool call."""

    name: str
    arguments: dict[str, Any]


@dataclass(frozen=True)
class ModelResponse:
    """One scripted `/chat/completions` response."""

    text: str = ''
    reasoning: str = ''
    tool_calls: tuple[ToolCall, ...] = ()
    # Substrings the request that receives this response must contain.
    request_contains: tuple[str, ...] = ()
    # Model faults: an HTTP status other than 200, a verbatim response body
    # that replaces the synthesized stream, and a connection abort at its end.
    status: int = 200
    raw: str | None = None
    abort: bool = False


@dataclass(frozen=True)
class Step:
    """One user message and the expectations on its reply turn."""

    send: str
    reply_contains: tuple[str, ...] = ()
    reply_matches: tuple[str, ...] = ()
    kinds: tuple[str, ...] = ()
    tool_contains: tuple[str, ...] = ()
    notice_contains: tuple[str, ...] = ()
    tool_errors_allowed: bool = False
    # Recorded mode: substrings some model request made during this step must
    # contain (from the first request carrying this step's message up to the
    # first request carrying the next one).
    request_contains: tuple[str, ...] = ()


@dataclass(frozen=True)
class HttpCheck:
    """One direct WebServer request made after the System is ready."""

    method: str
    path: str
    body: str | None
    status: int
    body_contains: tuple[str, ...] = ()
    # "before" runs after startup; "after" runs once the chat has settled.
    when: str = 'before'


@dataclass(frozen=True)
class Scenario:
    """A complete end-to-end scenario."""

    path: Path
    name: str
    description: str
    mode: str
    responses: tuple[ModelResponse, ...]
    tape: Path | None
    steps: tuple[Step, ...]
    expect_logs: tuple[str, ...]
    forbid_logs: tuple[str, ...]
    allow_logs: tuple[str, ...] = field(default=())
    await_logs: tuple[str, ...] = field(default=())
    # Patterns that must appear after startup before any request or chat.
    ready_logs: tuple[str, ...] = field(default=())
    await_seconds: float = 30.0
    http: tuple[HttpCheck, ...] = field(default=())
    # Maximum ordinary-heap high-water mark the System may report.
    heap_high_water_max: int | None = None
    # Virtual GPIO/I2C set up before the chat and checked after it.
    virtual_io: VirtualIoSpec | None = None

    @property
    def slug(self) -> str:
        """File-name stem used for recorded tapes and artifacts."""

        return self.path.stem


def load_scenario(path: Path) -> Scenario:
    """Parse and validate one TOML scenario file."""

    try:
        document = tomllib.loads(path.read_text(encoding='utf-8'))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise ScenarioError(f'{path}: {exc}') from exc

    name = _string(document, 'name', path)
    model = document.get('model', {'mode': 'none'})
    if not isinstance(model, dict):
        raise ScenarioError(f'{path}: model must be a table')
    mode = _string(model, 'mode', path)
    if mode not in ('scripted', 'recorded', 'none'):
        raise ScenarioError(
            f'{path}: model.mode must be "scripted", "recorded", or "none"'
        )

    responses: tuple[ModelResponse, ...] = ()
    tape: Path | None = None
    if mode == 'scripted':
        responses = tuple(
            _response(entry, path) for entry in _list(model, 'responses', path)
        )
        if not responses:
            raise ScenarioError(f'{path}: scripted model needs at least one response')
    else:
        tape = path.parent.parent / 'tapes' / f'{path.stem}.jsonl'

    steps = tuple(_step(entry, path) for entry in _list(document, 'steps', path))
    if mode != 'recorded' and any(step.request_contains for step in steps):
        raise ScenarioError(
            f'{path}: step request_contains is for recorded scenarios; '
            'scripted ones put it on [[model.responses]]'
        )
    http = tuple(_http(entry, path) for entry in _list(document, 'http', path))
    if not steps and not http:
        raise ScenarioError(f'{path}: scenario needs at least one step or http check')

    logs = document.get('logs', {})
    if not isinstance(logs, dict):
        raise ScenarioError(f'{path}: logs must be a table')
    memory = document.get('memory', {})
    if not isinstance(memory, dict):
        raise ScenarioError(f'{path}: memory must be a table')
    heap_max = memory.get('heap_high_water_max')
    if heap_max is not None and (not isinstance(heap_max, int) or heap_max <= 0):
        raise ScenarioError(f'{path}: memory.heap_high_water_max must be a byte count')
    virtual_io = None
    if 'virtual_io' in document:
        try:
            virtual_io = parse_spec(document['virtual_io'], path)
        except VirtualIoSpecError as exc:
            raise ScenarioError(str(exc)) from exc
    return Scenario(
        path=path,
        name=name,
        description=str(document.get('description', '')),
        mode=mode,
        responses=responses,
        tape=tape,
        steps=steps,
        expect_logs=_strings(logs, 'expect', path),
        forbid_logs=_strings(logs, 'forbid', path) or DEFAULT_FORBIDDEN_LOGS,
        allow_logs=_strings(logs, 'allow', path),
        await_logs=_strings(logs, 'await', path),
        ready_logs=_strings(logs, 'ready', path),
        await_seconds=float(logs.get('await_seconds', 30)),
        http=http,
        heap_high_water_max=heap_max,
        virtual_io=virtual_io,
    )


def discover(directory: Path) -> list[Path]:
    """Return every scenario file in `directory`, sorted by name."""

    return sorted(directory.glob('*.toml'))


def _response(entry: Any, path: Path) -> ModelResponse:
    if not isinstance(entry, dict):
        raise ScenarioError(f'{path}: each model response must be a table')
    calls = tuple(
        ToolCall(
            name=_string(call, 'name', path),
            arguments=dict(call.get('arguments', {})),
        )
        for call in entry.get('tool_calls', [])
    )
    text = str(entry.get('text', ''))
    raw = entry.get('raw')
    if raw is not None and not isinstance(raw, str):
        raise ScenarioError(f'{path}: a model response raw body must be a string')
    status = entry.get('status', 200)
    if not isinstance(status, int) or not 100 <= status <= 599:
        raise ScenarioError(f'{path}: a model response status must be an HTTP status')
    if raw is not None and (text or calls):
        raise ScenarioError(f'{path}: a raw model response cannot also have text or tool_calls')
    if raw is None and not text and not calls:
        raise ScenarioError(f'{path}: a model response needs text, tool_calls, or raw')
    return ModelResponse(
        text=text,
        reasoning=str(entry.get('reasoning', '')),
        tool_calls=calls,
        request_contains=_strings(entry, 'request_contains', path),
        status=status,
        raw=raw,
        abort=bool(entry.get('abort', False)),
    )


def _step(entry: Any, path: Path) -> Step:
    if not isinstance(entry, dict):
        raise ScenarioError(f'{path}: each step must be a table')
    return Step(
        send=_string(entry, 'send', path),
        reply_contains=_strings(entry, 'reply_contains', path),
        reply_matches=_strings(entry, 'reply_matches', path),
        kinds=_strings(entry, 'kinds', path),
        tool_contains=_strings(entry, 'tool_contains', path),
        notice_contains=_strings(entry, 'notice_contains', path),
        tool_errors_allowed=bool(entry.get('tool_errors_allowed', False)),
        request_contains=_strings(entry, 'request_contains', path),
    )


def _http(entry: Any, path: Path) -> HttpCheck:
    if not isinstance(entry, dict):
        raise ScenarioError(f'{path}: each http check must be a table')
    body = entry.get('body')
    if body is not None and not isinstance(body, str):
        raise ScenarioError(f'{path}: http body must be a string')
    return HttpCheck(
        method=str(entry.get('method', 'GET')),
        path=_string(entry, 'path', path),
        body=body,
        status=int(entry.get('status', 200)),
        body_contains=_strings(entry, 'body_contains', path),
        when=_when(entry, path),
    )


def _when(entry: dict[str, Any], path: Path) -> str:
    when = entry.get('when', 'before')
    if when not in ('before', 'after'):
        raise ScenarioError(f'{path}: http when must be "before" or "after"')
    return when


def _list(document: dict[str, Any], key: str, path: Path) -> list[Any]:
    value = document.get(key, [])
    if not isinstance(value, list):
        raise ScenarioError(f'{path}: {key} must be an array')
    return value


def _string(document: dict[str, Any], key: str, path: Path) -> str:
    value = document.get(key)
    if not isinstance(value, str) or not value:
        raise ScenarioError(f'{path}: {key} must be a non-empty string')
    return value


def _strings(document: dict[str, Any], key: str, path: Path) -> tuple[str, ...]:
    value = document.get(key, [])
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ScenarioError(f'{path}: {key} must be an array of strings')
    return tuple(value)
