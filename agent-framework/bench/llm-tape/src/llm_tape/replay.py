"""Strict sequential replay server with original timing only."""

from __future__ import annotations

import asyncio
import time
from pathlib import Path

from aiohttp import web

from .headers import to_multidict
from .tape import Interaction, Tape, load_tape

CONTROL_PATH = '/_llm_tape/health'
CURSOR_KEY = web.AppKey('cursor', 'ReplayCursor')


class ReplayCursor:
    """Consume a tape in recorded request order."""

    def __init__(self, tape: Tape):
        self._interactions = tape.interactions
        self._next_index = 0
        self._lock = asyncio.Lock()

    @property
    def consumed(self) -> int:
        return self._next_index

    @property
    def total(self) -> int:
        return len(self._interactions)

    async def match(self, *, method: str, path: str) -> tuple[Interaction | None, str]:
        """Match without consuming on exhaustion or method/path mismatch."""

        async with self._lock:
            if self._next_index >= len(self._interactions):
                return None, 'tape exhausted'
            interaction = self._interactions[self._next_index]
            expected = interaction.request
            if method != expected.method or path != expected.path:
                return (
                    None,
                    'request mismatch: '
                    f'expected {expected.method} {expected.path}, got {method} {path}',
                )
            self._next_index += 1
            return interaction, ''


def create_replay_app(tape: Path | str | Tape) -> web.Application:
    """Create an offline replay application for a validated tape."""

    loaded = tape if isinstance(tape, Tape) else load_tape(tape)
    app = web.Application(client_max_size=64 * 1024**2)
    app[CURSOR_KEY] = ReplayCursor(loaded)
    app.router.add_get(CONTROL_PATH, _health)
    app.router.add_route('*', '/{tail:.*}', _replay_request)
    return app


async def _health(request: web.Request) -> web.Response:
    cursor = request.app[CURSOR_KEY]
    return web.json_response(
        {
            'status': 'ok',
            'mode': 'replay',
            'consumed': cursor.consumed,
            'total': cursor.total,
        }
    )


async def _replay_request(request: web.Request) -> web.StreamResponse:
    started_ns = time.monotonic_ns()
    # Drain the request body even though replay matching is deliberately
    # content-agnostic. This keeps HTTP/1.1 connection reuse correct and makes
    # the recorded response offsets include the same request-upload phase as
    # record mode.
    await request.read()
    interaction, error = await request.app[CURSOR_KEY].match(
        method=request.method,
        path=request.path,
    )
    if interaction is None:
        return web.json_response(
            {'error': 'ReplayMismatch', 'message': error},
            status=409,
        )

    await _wait_until(started_ns, interaction.response_start.at_us)
    response = web.StreamResponse(
        status=interaction.response_start.status,
        reason=interaction.response_start.reason,
        headers=to_multidict(interaction.response_start.headers),
    )
    await response.prepare(request)

    try:
        for chunk in interaction.chunks:
            await _wait_until(started_ns, chunk.at_us)
            await response.write(chunk.data)
        await _wait_until(started_ns, interaction.response_end.at_us)

        if interaction.response_end.outcome == 'eof':
            await response.write_eof()
        else:
            transport = request.transport
            if transport is not None:
                transport.abort()
    except (ConnectionError, RuntimeError):
        pass
    return response


async def _wait_until(started_ns: int, at_us: int) -> None:
    target_ns = started_ns + at_us * 1_000
    remaining_ns = target_ns - time.monotonic_ns()
    if remaining_ns > 0:
        await asyncio.sleep(remaining_ns / 1_000_000_000)
