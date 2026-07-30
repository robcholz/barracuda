"""Standalone tool: export a ``claw_trace`` forest to the Chrome Trace Event
Format (loadable in ``chrome://tracing`` or https://ui.perfetto.dev).

This is **not** part of the ``claw_trace`` library — it is a separate consumer
of it. ``claw_trace`` stays a pure parsing/reconstruction lib with no dependency
on ``chrometrace``; all Chrome-specific translation lives here.

Mapping:

- each **span** -> a *complete* event (``X``): ``enter`` timestamp + duration, so
  the viewer renders the span tree as a flame chart (nesting comes from
  overlapping intervals on the same thread). An unclosed span becomes a
  *duration begin* (``B``) with no end.
- each **event** -> an *instant* event (``i``, thread scope). Only explicitly
  marked ``counter.<series>=<number>`` fields additionally become a *counter*
  event (``C``); ordinary numeric fields remain instant-event arguments.
- each ``flow_link`` event -> a Chrome flow (``s``/``f``) from its enclosing
  span to a span selected by logical task and, optionally, span name. Generic
  ``flow.arg.*`` fields are copied to the source span and flow endpoints.
- ``run.session`` selects a session process and requires ``run.system``;
  ``run.system`` by itself selects a system process; records with neither use
  the ``unattributed`` process. ``task`` -> thread (``tid``). The inherited
  context, ``target`` and custom fields ride along in ``args``. The system scope
  is part of a session process's identity.
"""

from __future__ import annotations

import os
import re
from dataclasses import dataclass
from typing import Callable

import chrometrace
from chrometrace import TraceEvent, TraceEventType

from claw_trace import EventNode, Forest, GroupedContext, SpanNode, flatten_context

__all__ = ['chrome_trace_events', 'write_chrome_trace']

# Chrome timestamps are microseconds; our trace timestamps are milliseconds.
_US_PER_MS = 1000
# pid/process name used only when neither a system nor a session is attributed.
_UNATTRIBUTED_PROCESS = 'unattributed'

# A loose ``key=value`` token (value has no spaces); used to lift fields out of
# the free-form custom context for nicer ``args`` and explicit counter series.
_KV_TOKEN = re.compile(r'^([^\s=]+)=(\S+)$')
_COUNTER_PREFIX = 'counter.'
_FLOW_LINK_EVENT = 'flow_link'
_FLOW_NAME_FIELD = 'flow.name'
_FLOW_TARGET_TASK_FIELD = 'flow.target_task'
_FLOW_TARGET_SPAN_FIELD = 'flow.target_span'
_FLOW_ARG_PREFIX = 'flow.arg.'
_FLOW_CATEGORY = 'flow'

# Resolves a (pid, tid) lane from a span/event's context + task.
_Lane = Callable[[GroupedContext, str], 'tuple[int, int]']


@dataclass(frozen=True, slots=True)
class _FlowLink:
    source_span_id: int
    name: str
    target_task: str
    target_span: str | None
    args: dict[str, str]


class _FlowTraceEvent:
    """Small adapter for Chrome flow fields unsupported by ``chrometrace``."""

    def __init__(self, body: dict[str, object]) -> None:
        self._body = body

    def to_dict(self) -> dict[str, object]:
        return dict(self._body)


class _IdAllocator:
    """Hands out stable small integer ids for hashable keys (pid / tid)."""

    def __init__(self, start: int = 1) -> None:
        self._next = start
        self._ids: dict[object, int] = {}

    def get(self, key: object) -> int:
        if key not in self._ids:
            self._ids[key] = self._next
            self._next += 1
        return self._ids[key]


def _loose_kv(text: str) -> dict[str, str]:
    """Best-effort split of free-form custom text into ``key=value`` tokens.

    Tokens that are not ``key=value`` are ignored; the original text is never
    required to be structured (the spec calls custom context free text).
    """
    fields: dict[str, str] = {}
    for token in text.split():
        match = _KV_TOKEN.match(token)
        if match is not None:
            fields[match.group(1)] = match.group(2)
    return fields


def _counter_series(fields: dict[str, str]) -> dict[str, float]:
    """Parse only explicitly marked ``counter.<series>=<number>`` fields."""
    series: dict[str, float] = {}
    for key, value in fields.items():
        if not key.startswith(_COUNTER_PREFIX):
            continue
        series_name = key.removeprefix(_COUNTER_PREFIX)
        if not series_name:
            raise ValueError('counter field requires a series name')
        try:
            series[series_name] = float(value)
        except ValueError:
            raise ValueError(f'{key} must be numeric, got {value!r}') from None
    return series


def _custom_args(custom: str) -> dict[str, object]:
    """Lift ``key=value`` tokens out of custom text; keep leftover as ``message``."""
    if not custom:
        return {}
    fields = _loose_kv(custom)
    args: dict[str, object] = dict(fields)
    if not fields:
        args['message'] = custom
    return args


def chrome_trace_events(forest: Forest) -> list[TraceEvent | _FlowTraceEvent]:
    """Translate ``forest`` into a flat list of ``chrometrace.TraceEvent``.

    Pure (no I/O): emits process/thread name metadata, one event per span, and
    instant events for each trace event, plus counters only for explicit
    ``counter.<series>`` fields. Ready to feed a :class:`chrometrace.TraceSink`
    or to inspect in tests via ``to_dict()``.
    """
    pids = _IdAllocator()
    tids = _IdAllocator()
    seen_pid: set[int] = set()
    seen_tid: set[tuple[int, int]] = set()
    out: list[TraceEvent | _FlowTraceEvent] = []

    def lane(context: GroupedContext, task: str) -> tuple[int, int]:
        """Resolve a scoped process/task lane, emitting naming metadata once."""
        run_context = context.get('run', {})
        system = run_context.get('system')
        session = run_context.get('session')
        if session is not None:
            if system is None:
                raise ValueError(
                    'invalid trace context: run.session requires run.system; '
                    'legacy traces are not supported'
                )
            process_key: object = ('session', system, session)
            process_name = session
        elif system is not None:
            process_key = ('system', system)
            process_name = system
        else:
            process_key = ('unattributed',)
            process_name = _UNATTRIBUTED_PROCESS

        pid = pids.get(process_key)
        tid = tids.get((process_key, task))
        if pid not in seen_pid:
            out.append(
                TraceEvent.process_name(process_id=pid, process_name=process_name)
            )
            seen_pid.add(pid)
        if (pid, tid) not in seen_tid:
            out.append(
                TraceEvent.thread_name(process_id=pid, thread_id=tid, thread_name=task)
            )
            seen_tid.add((pid, tid))
        return pid, tid

    flow_links = _flow_links(forest)
    flow_source_args = _flow_source_args(flow_links)
    for span in forest.spans.values():
        out.append(_span_event(span, lane, flow_source_args.get(span.id)))
    # Chrome flow starts bind to the most recent event on their lane. Emit
    # flows before ordinary instants so a same-timestamp instant cannot replace
    # the intended source span as the binding point.
    out.extend(_flow_events(forest, lane, flow_links))
    for event in forest.events:
        out.extend(_event_events(event, lane))
    return out


def _span_event(
    span: SpanNode,
    lane: _Lane,
    extra_args: dict[str, object] | None = None,
) -> TraceEvent:
    pid, tid = lane(span.context, span.task)
    args: dict[str, object] = {
        'span': span.id,
        'target': span.target,
        **flatten_context(span.context),
        **_custom_args(span.custom),
        **(extra_args or {}),
    }
    if span.parent_id is not None:
        args['parent'] = span.parent_id
    start_us = span.enter_ts * _US_PER_MS
    if span.duration_ms is not None:
        return TraceEvent.complete(
            name=span.name,
            timestamp_us=start_us,
            duration_us=span.duration_ms * _US_PER_MS,
            process_id=pid,
            thread_id=tid,
            categories=[span.target],
            args=args,
        )
    # Unclosed span: open-ended begin (the viewer extends it to the trace end).
    return TraceEvent.duration_begin(
        name=span.name,
        timestamp_us=start_us,
        process_id=pid,
        thread_id=tid,
        categories=[span.target],
        args=args,
    )


def _required_flow_field(fields: dict[str, str], name: str) -> str:
    value = fields.get(name)
    if not value:
        raise ValueError(f'{_FLOW_LINK_EVENT} requires {name}')
    return value


def _flow_links(forest: Forest) -> list[_FlowLink]:
    links: list[_FlowLink] = []
    for event in forest.events:
        if event.name != _FLOW_LINK_EVENT:
            continue
        if event.span_id is None:
            raise ValueError(f'{_FLOW_LINK_EVENT} requires an enclosing source span')
        fields = _loose_kv(event.custom)
        args: dict[str, str] = {}
        for key, value in fields.items():
            if not key.startswith(_FLOW_ARG_PREFIX):
                continue
            arg_name = key.removeprefix(_FLOW_ARG_PREFIX)
            if not arg_name:
                raise ValueError(f'{_FLOW_ARG_PREFIX}<key> requires a non-empty key')
            args[arg_name] = value
        links.append(
            _FlowLink(
                source_span_id=event.span_id,
                name=_required_flow_field(fields, _FLOW_NAME_FIELD),
                target_task=_required_flow_field(fields, _FLOW_TARGET_TASK_FIELD),
                target_span=fields.get(_FLOW_TARGET_SPAN_FIELD),
                args=args,
            )
        )
    return links


def _flow_source_args(links: list[_FlowLink]) -> dict[int, dict[str, object]]:
    source_args: dict[int, dict[str, object]] = {}
    for link in links:
        args = source_args.setdefault(link.source_span_id, {})
        for key, value in link.args.items():
            previous = args.get(key)
            if previous is None:
                args[key] = value
            elif isinstance(previous, list):
                previous.append(value)
            else:
                args[key] = [previous, value]
    return source_args


def _flow_events(
    forest: Forest,
    lane: _Lane,
    links: list[_FlowLink],
) -> list[_FlowTraceEvent]:
    flows: list[_FlowTraceEvent] = []
    spans = list(forest.spans.values())
    for flow_id, link in enumerate(links, start=1):
        source = forest.spans.get(link.source_span_id)
        if source is None:
            continue
        source_run = source.context.get('run', {})
        candidates = [
            span
            for span in spans
            if span.task == link.target_task
            and (link.target_span is None or span.name == link.target_span)
            and span.context.get('run', {}).get('system') == source_run.get('system')
            and span.context.get('run', {}).get('session') == source_run.get('session')
            and span.enter_ts >= source.enter_ts
            and (source.exit_ts is None or span.enter_ts <= source.exit_ts)
        ]
        if not candidates:
            continue
        target = min(candidates, key=lambda span: (span.enter_ts, span.id))
        source_pid, source_tid = lane(source.context, source.task)
        target_pid, target_tid = lane(target.context, target.task)
        args: dict[str, object] = {
            'source_span': source.id,
            'target_task': link.target_task,
            **link.args,
        }
        flows.extend(
            [
                _FlowTraceEvent(
                    {
                        'name': link.name,
                        'cat': _FLOW_CATEGORY,
                        'ph': 's',
                        'ts': source.enter_ts * _US_PER_MS,
                        'pid': source_pid,
                        'tid': source_tid,
                        'id': flow_id,
                        'args': args,
                    }
                ),
                _FlowTraceEvent(
                    {
                        'name': link.name,
                        'cat': _FLOW_CATEGORY,
                        'ph': 'f',
                        'ts': target.enter_ts * _US_PER_MS,
                        'pid': target_pid,
                        'tid': target_tid,
                        'id': flow_id,
                        'bp': 'e',
                        'args': args,
                    }
                ),
            ]
        )
    return flows


def _event_events(event: EventNode, lane: _Lane) -> list[TraceEvent]:
    pid, tid = lane(event.context, event.task)
    timestamp_us = event.ts * _US_PER_MS
    args: dict[str, object] = {
        'target': event.target,
        **flatten_context(event.context),
        **_custom_args(event.custom),
    }
    out = [
        TraceEvent(
            name=event.name,
            event_type=TraceEventType.INSTANT,
            timestamp_us=timestamp_us,
            process_id=pid,
            thread_id=tid,
            categories=event.target,
            args=args,
            scope='t',
        )
    ]
    series = _counter_series(_loose_kv(event.custom))
    if series:
        out.append(
            TraceEvent.counter(
                name=event.name,
                timestamp_us=timestamp_us,
                process_id=pid,
                thread_id=tid,
                args=series,
            )
        )
    return out


def write_chrome_trace(forest: Forest, path: os.PathLike[str] | str) -> int:
    """Write ``forest`` to ``path`` as a Chrome Trace Event JSON array.

    Returns the number of trace events written (metadata included).
    """
    events = chrome_trace_events(forest)
    with chrometrace.TraceSink(path) as sink:
        for event in events:
            sink.add_trace_event(event)
    return len(events)
