from __future__ import annotations

import asyncio

import aiohttp
from aiohttp import web

from llm_tape.recorder import create_recorder_app
from llm_tape.replay import create_replay_app
from llm_tape.tape import load_tape


async def _serve(app: web.Application) -> tuple[web.AppRunner, str]:
    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, '127.0.0.1', 0)
    await site.start()
    sockets = site._server.sockets
    port = sockets[0].getsockname()[1]
    return runner, f'http://127.0.0.1:{port}'


async def test_record_then_replay_is_content_agnostic(tmp_path):
    source_chunks = [
        b'event: content_block_delta\ndata: {"text":"\xe4',
        b'\xb8\xad',
        b'"}\n\ndata: [DONE]\n\n',
    ]
    received_request = {}

    async def upstream_handler(request: web.Request) -> web.StreamResponse:
        received_request['body'] = await request.read()
        received_request['authorization'] = request.headers['authorization']
        response = web.StreamResponse(
            status=200,
            headers={'content-type': 'text/event-stream'},
        )
        await response.prepare(request)
        for chunk in source_chunks:
            await asyncio.sleep(0.01)
            await response.write(chunk)
        await response.write_eof()
        return response

    upstream_app = web.Application()
    upstream_app.router.add_post('/v1/messages', upstream_handler)
    upstream_runner, upstream_url = await _serve(upstream_app)

    tape_path = tmp_path / 'run.llmtape'
    recorder_runner, recorder_url = await _serve(
        create_recorder_app(upstream=upstream_url, output=tape_path)
    )
    request_body = b'{"stream":true,"messages":[]}'
    try:
        async with aiohttp.ClientSession() as session:
            async with session.post(
                f'{recorder_url}/v1/messages',
                data=request_body,
                headers={
                    'authorization': 'Bearer secret',
                    'content-type': 'application/json',
                },
            ) as response:
                recorded_body = await response.read()
                assert response.status == 200
    finally:
        await recorder_runner.cleanup()
        await upstream_runner.cleanup()

    assert received_request == {
        'body': request_body,
        'authorization': 'Bearer secret',
    }
    assert recorded_body == b''.join(source_chunks)

    tape = load_tape(tape_path)
    interaction = tape.interactions[0]
    assert interaction.request.body_size == len(request_body)
    assert ('authorization', '***') in [
        (name.lower(), value) for name, value in interaction.request.headers
    ]
    assert b''.join(chunk.data for chunk in interaction.chunks) == recorded_body
    assert len(interaction.chunks) >= 2

    replay_runner, replay_url = await _serve(create_replay_app(tape))
    try:
        async with aiohttp.ClientSession() as session:
            async with session.post(
                f'{replay_url}/v1/messages', data=b'content is deliberately ignored'
            ) as response:
                replayed_body = await response.read()
                assert response.status == 200
                assert response.headers['content-type'] == 'text/event-stream'
    finally:
        await replay_runner.cleanup()

    assert replayed_body == recorded_body
