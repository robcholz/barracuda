"""Standalone tool: export a ``claw_trace`` forest to the Chrome Trace Event
Format (loadable in ``chrome://tracing`` or https://ui.perfetto.dev).

This is **not** part of the ``claw_trace`` library — it is a separate consumer
of it. ``claw_trace`` stays a pure parsing/reconstruction lib with no dependency
on ``chrometrace``; all Chrome-specific translation lives here.

Mapping:

- each **span** -> a *complete* event (``X``). An unclosed span extends to the
  observed trace end (bounded by a closed ancestor) and carries
  ``incomplete=true``.
- each cross-lane **span parent** edge -> a standard Chrome flow (``s``/``f``)
  named ``span_parent``. Same-lane parent edges are represented by nested
  complete-event intervals.
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
_SPAN_PARENT_FLOW_NAME = 'span_parent'
# Give each generated flow proxy a distinct nanosecond-scale timestamp on
# either side of its target. TRACE input is millisecond-resolution, so this
# avoids collisions without changing any observed record timestamp.
_FLOW_ANCHOR_STEP_US = 0.001

# Resolves a (pid, tid) lane from a span/event's context + task.
_Lane = Callable[[GroupedContext, str], 'tuple[int, int]']


@dataclass(frozen=True, slots=True)
class _FlowLink:
    source_span_id: int
    name: str
    target_task: str
    target_span: str | None
    args: dict[str, str]


@dataclass(frozen=True, slots=True)
class _ResolvedFlow:
    source: SpanNode
    target: SpanNode
    name: str
    args: dict[str, object]


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


def _process_scope(context: GroupedContext) -> tuple[object, str]:
    """Return the stable process key and display name for effective context."""
    run_context = context.get('run', {})
    system = run_context.get('system')
    session = run_context.get('session')
    if session is not None:
        if system is None:
            raise ValueError(
                'invalid trace context: run.session requires run.system; '
                'legacy traces are not supported'
            )
        return ('session', system, session), session
    if system is not None:
        return ('system', system), system
    return ('unattributed',), _UNATTRIBUTED_PROCESS


def _lane_key(context: GroupedContext, task: str) -> tuple[object, str]:
    process_key, _ = _process_scope(context)
    return process_key, task


def _trace_end_ms(forest: Forest) -> int:
    """Return the latest timestamp observed anywhere in a reconstructed trace."""
    timestamps = [event.ts for event in forest.events]
    for span in forest.spans.values():
        timestamps.append(span.enter_ts)
        if span.exit_ts is not None:
            timestamps.append(span.exit_ts)
    return max(timestamps, default=0)


def _effective_exit_ts(forest: Forest, span: SpanNode, trace_end: int) -> int:
    """Close an incomplete span at the observable boundary of its parent tree."""
    if span.exit_ts is not None:
        return span.exit_ts

    effective_end = trace_end
    parent_id = span.parent_id
    visited = {span.id}
    while parent_id is not None and parent_id not in visited:
        visited.add(parent_id)
        parent = forest.spans.get(parent_id)
        if parent is None:
            break
        if parent.exit_ts is not None:
            effective_end = min(effective_end, parent.exit_ts)
        parent_id = parent.parent_id
    return max(span.enter_ts, effective_end)


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
        process_key, process_name = _process_scope(context)

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
    resolved_flows = [
        (flow_id, flow)
        for flow_id, flow in enumerate(
            [*_span_parent_flows(forest), *_resolve_explicit_flows(forest, flow_links)],
            start=1,
        )
    ]

    trace_end = _trace_end_ms(forest)
    for span in forest.spans.values():
        out.append(
            _span_event(
                span,
                lane,
                _effective_exit_ts(forest, span, trace_end),
                extra_args=flow_source_args.get(span.id),
            )
        )
    # Legacy Chrome flow endpoints cannot name source/target slice ids. Emit
    # dedicated instant proxies on both lanes around the target timestamp; the
    # proxies carry the authoritative source_span/target_span relationship.
    out.extend(_flow_events(resolved_flows, lane))
    for event in forest.events:
        out.extend(_event_events(event, lane))
    return out


def _span_event(
    span: SpanNode,
    lane: _Lane,
    exit_ts: int,
    extra_args: dict[str, object] | None = None,
) -> TraceEvent:
    pid, tid = lane(span.context, span.task)
    args: dict[str, object] = {
        'target': span.target,
        **flatten_context(span.context),
        **_custom_args(span.custom),
        **(extra_args or {}),
        # Structural identity is authoritative over any same-named display arg.
        'span': span.id,
    }
    if span.parent_id is not None:
        args['parent'] = span.parent_id
    if span.exit_ts is None:
        args['incomplete'] = True
    start_us = span.enter_ts * _US_PER_MS
    return TraceEvent.complete(
        name=span.name,
        timestamp_us=start_us,
        duration_us=(exit_ts - span.enter_ts) * _US_PER_MS,
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
    flows: list[tuple[int, _ResolvedFlow]],
    lane: _Lane,
) -> list[_FlowTraceEvent]:
    events: list[_FlowTraceEvent] = []
    for flow_id, flow in flows:
        source_pid, source_tid = lane(flow.source.context, flow.source.task)
        target_pid, target_tid = lane(flow.target.context, flow.target.task)
        target_start_us = flow.target.enter_ts * _US_PER_MS
        offset_us = flow_id * 3 * _FLOW_ANCHOR_STEP_US
        before_target = target_start_us - offset_us
        anchor_ts = (
            before_target
            if before_target >= flow.source.enter_ts * _US_PER_MS
            else target_start_us + offset_us
        )
        target_anchor_ts = max(
            target_start_us + offset_us + _FLOW_ANCHOR_STEP_US,
            anchor_ts + _FLOW_ANCHOR_STEP_US,
        )
        args: dict[str, object] = {
            'source_span': flow.source.id,
            'target_span': flow.target.id,
            **flow.args,
        }
        events.extend(
            [
                _FlowTraceEvent(
                    {
                        'name': f'{flow.source.name} → {flow.name}',
                        'cat': _FLOW_CATEGORY,
                        'ph': 'I',
                        'ts': anchor_ts,
                        'pid': source_pid,
                        'tid': source_tid,
                        's': 't',
                        'args': {
                            'flow_anchor': True,
                            'flow_anchor_role': 'source',
                            'source_name': flow.source.name,
                            'target_name': flow.target.name,
                            **args,
                        },
                    }
                ),
                _FlowTraceEvent(
                    {
                        'name': flow.name,
                        'cat': _FLOW_CATEGORY,
                        'ph': 's',
                        'ts': anchor_ts,
                        'pid': source_pid,
                        'tid': source_tid,
                        'id': flow_id,
                        'args': args,
                    }
                ),
                _FlowTraceEvent(
                    {
                        'name': f'{flow.name} → {flow.target.name}',
                        'cat': _FLOW_CATEGORY,
                        'ph': 'I',
                        'ts': target_anchor_ts,
                        'pid': target_pid,
                        'tid': target_tid,
                        's': 't',
                        'args': {
                            'flow_anchor': True,
                            'flow_anchor_role': 'target',
                            'source_name': flow.source.name,
                            'target_name': flow.target.name,
                            **args,
                        },
                    }
                ),
                _FlowTraceEvent(
                    {
                        'name': flow.name,
                        'cat': _FLOW_CATEGORY,
                        'ph': 'f',
                        'ts': target_anchor_ts,
                        'pid': target_pid,
                        'tid': target_tid,
                        'id': flow_id,
                        'bp': 'e',
                        'args': args,
                    }
                ),
            ]
        )
    return events


def _span_parent_flows(forest: Forest) -> list[_ResolvedFlow]:
    """Project parent edges that cannot be represented by same-lane nesting."""
    flows: list[_ResolvedFlow] = []
    for child in forest.spans.values():
        if child.parent_id is None:
            continue
        parent = forest.spans.get(child.parent_id)
        if parent is None or _lane_key(parent.context, parent.task) == _lane_key(
            child.context, child.task
        ):
            continue
        flows.append(
            _ResolvedFlow(
                source=parent,
                target=child,
                name=_SPAN_PARENT_FLOW_NAME,
                args={
                    'parent_span': parent.id,
                    'child_span': child.id,
                },
            )
        )
    return flows


def _resolve_explicit_flows(
    forest: Forest, links: list[_FlowLink]
) -> list[_ResolvedFlow]:
    resolved: list[_ResolvedFlow] = []
    spans = list(forest.spans.values())
    for link in links:
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
        args: dict[str, object] = {
            'source_span': source.id,
            'target_span': target.id,
            'target_task': link.target_task,
            **link.args,
        }
        resolved.append(
            _ResolvedFlow(
                source=source,
                target=target,
                name=link.name,
                args=args,
            )
        )
    return resolved


def _event_events(event: EventNode, lane: _Lane) -> list[TraceEvent]:
    pid, tid = lane(event.context, event.task)
    timestamp_us = event.ts * _US_PER_MS
    args: dict[str, object] = {
        'target': event.target,
        **flatten_context(event.context),
        **_custom_args(event.custom),
    }
    if event.span_id is not None:
        # Preserve the event's structural anchor for joins such as
        # event.args.span == child_span.args.parent.
        args['span'] = event.span_id
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
