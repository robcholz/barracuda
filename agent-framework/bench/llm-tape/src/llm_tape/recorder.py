"""Content-agnostic reverse proxy that records response byte chunks."""

from __future__ import annotations

import asyncio
import hashlib
import time
from collections.abc import AsyncIterator
from pathlib import Path

import aiohttp
from aiohttp import web
from loguru import logger

from .headers import (
    forwarded_request_headers,
    response_headers,
    stored_request_headers,
    to_multidict,
)
from .tape import TapeWriter

CONTROL_PATH = '/_llm_tape/health'
DECODED_REQUEST_ENCODINGS = frozenset({'br', 'deflate', 'gzip', 'zstd'})

UPSTREAM_KEY = web.AppKey('upstream', str)
SESSION_KEY = web.AppKey('session', aiohttp.ClientSession)
WRITER_KEY = web.AppKey('writer', TapeWriter)


def create_recorder_app(*, upstream: str, output: Path | str) -> web.Application:
    """Create a reverse-proxy application recording to a new tape file."""

    normalized_upstream = _normalize_upstream(upstream)
    writer = TapeWriter(output)
    app = web.Application(client_max_size=64 * 1024**2)
    app[UPSTREAM_KEY] = normalized_upstream
    app[WRITER_KEY] = writer
    app.cleanup_ctx.append(_client_session_context)
    app.on_cleanup.append(_close_writer)
    app.router.add_get(CONTROL_PATH, _health)
    app.router.add_route('*', '/{tail:.*}', _record_request)
    return app


async def _client_session_context(app: web.Application) -> AsyncIterator[None]:
    timeout = aiohttp.ClientTimeout(total=None, sock_connect=30, sock_read=None)
    async with aiohttp.ClientSession(
        auto_decompress=False,
        timeout=timeout,
        cookie_jar=aiohttp.DummyCookieJar(),
    ) as session:
        app[SESSION_KEY] = session
        yield


async def _close_writer(app: web.Application) -> None:
    writer = app[WRITER_KEY]
    await writer.close()
    logger.info('record tape_closed output={}', writer.path)


async def _health(request: web.Request) -> web.Response:
    del request
    return web.json_response({'status': 'ok', 'mode': 'record'})


async def _record_request(request: web.Request) -> web.StreamResponse:
    started_ns = time.monotonic_ns()
    logger.info(
        'record request_received method={} path={}',
        request.method,
        request.path,
    )
    body = await request.read()
    body_hash = hashlib.sha256(body).hexdigest()
    writer = request.app[WRITER_KEY]
    interaction_id, call_index = await writer.request(
        method=request.method,
        path=request.path,
        path_qs=request.raw_path,
        headers=stored_request_headers(request.raw_headers),
        body_sha256=body_hash,
        body_size=len(body),
    )
    logger.info(
        'record request_started interaction={} call_index={} method={} path={} '
        'request_bytes={}',
        interaction_id,
        call_index,
        request.method,
        request.path,
        len(body),
    )

    content_encoding = request.headers.get('Content-Encoding', '').lower()
    forwarded_headers = forwarded_request_headers(
        request.raw_headers,
        decoded_request_body=content_encoding in DECODED_REQUEST_ENCODINGS,
    )
    upstream_url = request.app[UPSTREAM_KEY] + request.raw_path

    try:
        upstream_response = await request.app[SESSION_KEY].request(
            method=request.method,
            url=upstream_url,
            headers=forwarded_headers,
            data=body,
            allow_redirects=False,
        )
    except (aiohttp.ClientError, asyncio.TimeoutError) as exc:
        logger.error(
            'record upstream_failed interaction={} error_type={} error={}',
            interaction_id,
            type(exc).__name__,
            exc,
        )
        return await _record_proxy_error(
            writer=writer,
            interaction_id=interaction_id,
            started_ns=started_ns,
            message=f'upstream request failed: {type(exc).__name__}: {exc}',
        )

    headers = response_headers(upstream_response.raw_headers)
    response_started_us = _elapsed_us(started_ns)
    await writer.response_start(
        interaction_id,
        at_us=response_started_us,
        status=upstream_response.status,
        reason=upstream_response.reason,
        headers=headers,
    )
    logger.info(
        'record response_started interaction={} status={} at_us={}',
        interaction_id,
        upstream_response.status,
        response_started_us,
    )
    downstream = web.StreamResponse(
        status=upstream_response.status,
        reason=upstream_response.reason,
        headers=to_multidict(headers),
    )
    await downstream.prepare(request)

    outcome = 'eof'
    sequence = 0
    response_bytes = 0
    try:
        async for chunk in upstream_response.content.iter_any():
            if not chunk:
                continue
            chunk_at_us = _elapsed_us(started_ns)
            await writer.chunk(
                interaction_id,
                seq=sequence,
                at_us=chunk_at_us,
                data=bytes(chunk),
            )
            response_bytes += len(chunk)
            logger.debug(
                'record chunk interaction={} seq={} bytes={} at_us={}',
                interaction_id,
                sequence,
                len(chunk),
                chunk_at_us,
            )
            sequence += 1
            try:
                await downstream.write(chunk)
            except ConnectionError:
                outcome = 'client_disconnect'
                logger.warning(
                    'record client_disconnected interaction={} after_chunks={}',
                    interaction_id,
                    sequence,
                )
                break
    except (aiohttp.ClientError, asyncio.TimeoutError) as exc:
        outcome = 'upstream_error'
        logger.error(
            'record upstream_stream_failed interaction={} error_type={} error={}',
            interaction_id,
            type(exc).__name__,
            exc,
        )
    except asyncio.CancelledError:
        outcome = 'client_disconnect'
        logger.warning('record request_cancelled interaction={}', interaction_id)
        raise
    finally:
        upstream_response.release()
        elapsed_us = _elapsed_us(started_ns)
        await writer.response_end(
            interaction_id,
            at_us=elapsed_us,
            outcome=outcome,
        )
        logger.info(
            'record request_completed interaction={} status={} chunks={} '
            'response_bytes={} elapsed_us={} outcome={}',
            interaction_id,
            upstream_response.status,
            sequence,
            response_bytes,
            elapsed_us,
            outcome,
        )

    try:
        await downstream.write_eof()
    except (ConnectionError, RuntimeError):
        pass
    return downstream


async def _record_proxy_error(
    *,
    writer: TapeWriter,
    interaction_id: str,
    started_ns: int,
    message: str,
) -> web.Response:
    body = message.encode('utf-8', errors='replace')
    headers = [
        ('Content-Type', 'text/plain; charset=utf-8'),
        ('Content-Length', str(len(body))),
    ]
    response_started_us = _elapsed_us(started_ns)
    await writer.response_start(
        interaction_id,
        at_us=response_started_us,
        status=502,
        reason='Bad Gateway',
        headers=headers,
    )
    chunk_at_us = _elapsed_us(started_ns)
    await writer.chunk(
        interaction_id,
        seq=0,
        at_us=chunk_at_us,
        data=body,
    )
    response_ended_us = _elapsed_us(started_ns)
    await writer.response_end(
        interaction_id,
        at_us=response_ended_us,
        outcome='eof',
    )
    logger.info(
        'record request_completed interaction={} status=502 chunks=1 '
        'response_bytes={} elapsed_us={} outcome=proxy_error',
        interaction_id,
        len(body),
        response_ended_us,
    )
    return web.Response(status=502, reason='Bad Gateway', headers=headers, body=body)


def _normalize_upstream(upstream: str) -> str:
    stripped = upstream.rstrip('/')
    if not stripped.startswith(('http://', 'https://')):
        raise ValueError('upstream must start with http:// or https://')
    return stripped


def _elapsed_us(started_ns: int) -> int:
    return (time.monotonic_ns() - started_ns) // 1_000
