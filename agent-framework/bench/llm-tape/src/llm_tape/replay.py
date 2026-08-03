"""Strict sequential replay server with original timing only."""

from __future__ import annotations

import asyncio
import time
from pathlib import Path

from aiohttp import web
from loguru import logger

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
    logger.info(
        'replay tape_loaded version={} interactions={} created_at={}',
        loaded.version,
        len(loaded.interactions),
        loaded.created_at,
    )
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
    cursor = request.app[CURSOR_KEY]
    logger.info(
        'replay request_started method={} path={} next_index={} total={}',
        request.method,
        request.path,
        cursor.consumed,
        cursor.total,
    )
    # Drain the request body even though replay matching is deliberately
    # content-agnostic. This keeps HTTP/1.1 connection reuse correct and makes
    # the recorded response offsets include the same request-upload phase as
    # record mode.
    await request.read()
    interaction, error = await cursor.match(
        method=request.method,
        path=request.path,
    )
    if interaction is None:
        logger.warning(
            'replay request_rejected method={} path={} next_index={} reason={}',
            request.method,
            request.path,
            cursor.consumed,
            error,
        )
        return web.json_response(
            {'error': 'ReplayMismatch', 'message': error},
            status=409,
        )

    recorded_request = interaction.request
    logger.info(
        'replay request_matched interaction={} call_index={} status={} chunks={} '
        'recorded_end_us={}',
        recorded_request.interaction_id,
        recorded_request.call_index,
        interaction.response_start.status,
        len(interaction.chunks),
        interaction.response_end.at_us,
    )
    await _wait_until(started_ns, interaction.response_start.at_us)
    response = web.StreamResponse(
        status=interaction.response_start.status,
        reason=interaction.response_start.reason,
        headers=to_multidict(interaction.response_start.headers),
    )
    await response.prepare(request)

    chunks_written = 0
    response_bytes = 0
    outcome = interaction.response_end.outcome
    try:
        for chunk in interaction.chunks:
            await _wait_until(started_ns, chunk.at_us)
            await response.write(chunk.data)
            chunks_written += 1
            response_bytes += len(chunk.data)
            logger.debug(
                'replay chunk interaction={} seq={} bytes={} at_us={}',
                recorded_request.interaction_id,
                chunk.seq,
                len(chunk.data),
                chunk.at_us,
            )
        await _wait_until(started_ns, interaction.response_end.at_us)

        if interaction.response_end.outcome == 'eof':
            await response.write_eof()
        else:
            transport = request.transport
            if transport is not None:
                transport.abort()
    except (ConnectionError, RuntimeError) as exc:
        outcome = 'client_disconnect'
        logger.warning(
            'replay client_disconnected interaction={} after_chunks={} '
            'error_type={} error={}',
            recorded_request.interaction_id,
            chunks_written,
            type(exc).__name__,
            exc,
        )
    except asyncio.CancelledError:
        outcome = 'client_disconnect'
        logger.warning(
            'replay request_cancelled interaction={} after_chunks={}',
            recorded_request.interaction_id,
            chunks_written,
        )
        raise
    finally:
        logger.info(
            'replay request_completed interaction={} status={} chunks={} '
            'response_bytes={} elapsed_us={} outcome={}',
            recorded_request.interaction_id,
            interaction.response_start.status,
            chunks_written,
            response_bytes,
            _elapsed_us(started_ns),
            outcome,
        )
    return response


async def _wait_until(started_ns: int, at_us: int) -> None:
    target_ns = started_ns + at_us * 1_000
    remaining_ns = target_ns - time.monotonic_ns()
    if remaining_ns > 0:
        await asyncio.sleep(remaining_ns / 1_000_000_000)


def _elapsed_us(started_ns: int) -> int:
    return (time.monotonic_ns() - started_ns) // 1_000
