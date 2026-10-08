from __future__ import annotations

import json
import time

import aiohttp
from aiohttp import web

from llm_tape.keys import conversation_key
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


async def test_capture_requests_writes_matched_bodies(tmp_path):
    path = tmp_path / 'run.llmtape'
    capture = tmp_path / 'requests'
    await _make_tape(path, response_at_us=0, chunk_at_us=0)
    runner, base_url = await _serve(create_replay_app(path, capture))
    try:
        async with aiohttp.ClientSession() as session:
            async with session.post(
                f'{base_url}/v1/messages', data=b'{"prompt":"hi"}'
            ) as response:
                await response.read()
        assert (capture / 'call-000000.body').read_bytes() == b'{"prompt":"hi"}'
    finally:
        await runner.cleanup()


def _chat(first_user: str) -> bytes:
    return json.dumps(
        {
            'messages': [
                {'role': 'system', 'content': 's'},
                {'role': 'user', 'content': first_user},
            ]
        }
    ).encode()


async def _make_keyed_tape(path, recorded: list[tuple[str, bytes]]):
    writer = TapeWriter(path)
    for first_user, response in recorded:
        interaction_id, _ = await writer.request(
            method='POST',
            path='/v1/chat/completions',
            path_qs='/v1/chat/completions',
            headers=[],
            body_sha256='0' * 64,
            body_size=0,
            match_key=conversation_key(_chat(first_user)),
        )
        await writer.response_start(
            interaction_id, at_us=0, status=200, reason='OK', headers=[]
        )
        await writer.chunk(interaction_id, seq=0, at_us=0, data=response)
        await writer.response_end(interaction_id, at_us=0, outcome='eof')
    await writer.close()


async def test_keyed_tape_serves_each_conversation_in_its_own_order(tmp_path):
    path = tmp_path / 'run.llmtape'
    await _make_keyed_tape(
        path, [('root', b'root-1'), ('child', b'child-1'), ('root', b'root-2')]
    )
    runner, base_url = await _serve(create_replay_app(path))
    try:
        async with aiohttp.ClientSession() as session:
            served = []
            for first_user in ['root', 'stranger', 'root', 'child']:
                async with session.post(
                    f'{base_url}/v1/chat/completions', data=_chat(first_user)
                ) as response:
                    if first_user == 'stranger':
                        assert response.status == 409
                        message = (await response.json())['message']
                        assert 'no recorded request left' in message
                        continue
                    assert response.status == 200
                    served.append(await response.read())
        assert served == [b'root-1', b'root-2', b'child-1']
    finally:
        await runner.cleanup()


def test_conversation_key_reads_openai_and_anthropic_bodies():
    openai = conversation_key(_chat('hello'))
    anthropic = conversation_key(
        json.dumps(
            {
                'system': 's',
                'messages': [
                    {'role': 'user', 'content': [{'type': 'text', 'text': 'hello'}]}
                ],
            }
        ).encode()
    )
    assert openai is not None
    assert openai == anthropic
    assert conversation_key(_chat('other')) != openai
    assert conversation_key(b'not json') is None
    assert conversation_key(b'{"model": "x"}') is None
