from __future__ import annotations

import time

import aiohttp
from aiohttp import web

from llm_tape.replay import create_replay_app
from llm_tape.tape import TapeWriter


async def _serve(app: web.Application) -> tuple[web.AppRunner, str]:
    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, '127.0.0.1', 0)
    await site.start()
    sockets = site._server.sockets
    port = sockets[0].getsockname()[1]
    return runner, f'http://127.0.0.1:{port}'


async def _make_tape(path, *, response_at_us=10_000, chunk_at_us=30_000):
    writer = TapeWriter(path)
    interaction_id, _ = await writer.request(
        method='POST',
        path='/v1/messages',
        path_qs='/v1/messages',
        headers=[],
        body_sha256='0' * 64,
        body_size=0,
    )
    await writer.response_start(
        interaction_id,
        at_us=response_at_us,
        status=201,
        reason='Created',
        headers=[('content-type', 'application/octet-stream')],
    )
    await writer.chunk(
        interaction_id,
        seq=0,
        at_us=chunk_at_us,
        data=b'raw\x00bytes',
    )
    await writer.response_end(
        interaction_id,
        at_us=chunk_at_us + 5_000,
        outcome='eof',
    )
    await writer.close()


async def test_replay_returns_raw_bytes_with_original_timing(tmp_path):
    path = tmp_path / 'run.llmtape'
    await _make_tape(path)
    runner, base_url = await _serve(create_replay_app(path))
    try:
        started = time.monotonic()
        async with aiohttp.ClientSession() as session:
            async with session.post(f'{base_url}/v1/messages') as response:
                body = await response.read()
                elapsed = time.monotonic() - started
                assert response.status == 201
                assert response.reason == 'Created'
                assert response.headers['content-type'] == 'application/octet-stream'
        assert body == b'raw\x00bytes'
        assert elapsed >= 0.03
    finally:
        await runner.cleanup()


async def test_mismatch_does_not_consume_interaction(tmp_path, log_messages):
    path = tmp_path / 'run.llmtape'
    await _make_tape(path, response_at_us=0, chunk_at_us=0)
    runner, base_url = await _serve(create_replay_app(path))
    try:
        async with aiohttp.ClientSession() as session:
            async with session.get(f'{base_url}/wrong') as mismatch:
                payload = await mismatch.json()
                assert mismatch.status == 409
                assert payload['error'] == 'ReplayMismatch'

            async with session.post(f'{base_url}/v1/messages') as replayed:
                assert replayed.status == 201
                assert await replayed.read() == b'raw\x00bytes'

            async with session.post(f'{base_url}/v1/messages') as exhausted:
                payload = await exhausted.json()
                assert exhausted.status == 409
                assert payload['message'] == 'tape exhausted'
    finally:
        await runner.cleanup()

    log_text = ''.join(log_messages)
    assert 'reason=request mismatch:' in log_text
    assert 'reason=tape exhausted' in log_text


async def test_health_does_not_consume_interaction(tmp_path):
    path = tmp_path / 'run.llmtape'
    await _make_tape(path, response_at_us=0, chunk_at_us=0)
    runner, base_url = await _serve(create_replay_app(path))
    try:
        async with aiohttp.ClientSession() as session:
            async with session.get(f'{base_url}/_llm_tape/health') as health:
                assert await health.json() == {
                    'status': 'ok',
                    'mode': 'replay',
                    'consumed': 0,
                    'total': 1,
                }
            async with session.post(f'{base_url}/v1/messages') as replayed:
                assert replayed.status == 201
    finally:
        await runner.cleanup()
