"""Content-agnostic reverse proxy that records response byte chunks."""

from __future__ import annotations

import asyncio
import hashlib
import time
from collections.abc import AsyncIterator
from pathlib import Path

import aiohttp
from aiohttp import web

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
    await app[WRITER_KEY].close()


async def _health(request: web.Request) -> web.Response:
    del request
    return web.json_response({'status': 'ok', 'mode': 'record'})


async def _record_request(request: web.Request) -> web.StreamResponse:
    started_ns = time.monotonic_ns()
    body = await request.read()
    body_hash = hashlib.sha256(body).hexdigest()
    writer = request.app[WRITER_KEY]
    interaction_id, _ = await writer.request(
        method=request.method,
        path=request.path,
        path_qs=request.raw_path,
        headers=stored_request_headers(request.raw_headers),
        body_sha256=body_hash,
        body_size=len(body),
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
        return await _record_proxy_error(
            writer=writer,
            interaction_id=interaction_id,
            started_ns=started_ns,
            message=f'upstream request failed: {type(exc).__name__}: {exc}',
        )

    headers = response_headers(upstream_response.raw_headers)
    await writer.response_start(
        interaction_id,
        at_us=_elapsed_us(started_ns),
        status=upstream_response.status,
        reason=upstream_response.reason,
        headers=headers,
    )
    downstream = web.StreamResponse(
        status=upstream_response.status,
        reason=upstream_response.reason,
        headers=to_multidict(headers),
    )
    await downstream.prepare(request)

    outcome = 'eof'
    sequence = 0
    try:
        async for chunk in upstream_response.content.iter_any():
            if not chunk:
                continue
            await writer.chunk(
                interaction_id,
                seq=sequence,
                at_us=_elapsed_us(started_ns),
                data=bytes(chunk),
            )
            sequence += 1
            try:
                await downstream.write(chunk)
            except ConnectionError:
                outcome = 'client_disconnect'
                break
    except (aiohttp.ClientError, asyncio.TimeoutError):
        outcome = 'upstream_error'
    except asyncio.CancelledError:
        outcome = 'client_disconnect'
        raise
    finally:
        upstream_response.release()
        await writer.response_end(
            interaction_id,
            at_us=_elapsed_us(started_ns),
            outcome=outcome,
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
    await writer.response_start(
        interaction_id,
        at_us=_elapsed_us(started_ns),
        status=502,
        reason='Bad Gateway',
        headers=headers,
    )
    await writer.chunk(
        interaction_id,
        seq=0,
        at_us=_elapsed_us(started_ns),
        data=body,
    )
    await writer.response_end(
        interaction_id,
        at_us=_elapsed_us(started_ns),
        outcome='eof',
    )
    return web.Response(status=502, reason='Bad Gateway', headers=headers, body=body)


def _normalize_upstream(upstream: str) -> str:
    stripped = upstream.rstrip('/')
    if not stripped.startswith(('http://', 'https://')):
        raise ValueError('upstream must start with http:// or https://')
    return stripped


def _elapsed_us(started_ns: int) -> int:
    return (time.monotonic_ns() - started_ns) // 1_000
