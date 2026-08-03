from __future__ import annotations

import json

import pytest

from llm_tape.tape import TapeFormatError, TapeWriter, load_tape


async def test_tape_round_trips_arbitrary_chunk_bytes(tmp_path):
    path = tmp_path / 'run.llmtape'
    writer = TapeWriter(path)
    interaction_id, call_index = await writer.request(
        method='POST',
        path='/v1/messages',
        path_qs='/v1/messages?beta=true',
        headers=[('authorization', '***')],
        body_sha256='0' * 64,
        body_size=17,
    )
    assert call_index == 0
    await writer.response_start(
        interaction_id,
        at_us=10,
        status=200,
        reason='OK',
        headers=[('content-type', 'text/event-stream')],
    )
    chunks = [b'data: {"text":"\xe4', b'\xb8\xad"}\n', b'\ndata: [DONE]\n\n']
    for seq, chunk in enumerate(chunks):
        await writer.chunk(
            interaction_id,
            seq=seq,
            at_us=20 + seq * 10,
            data=chunk,
        )
    await writer.response_end(interaction_id, at_us=60, outcome='eof')
    await writer.close()

    tape = load_tape(path)

    assert tape.version == 1
    assert len(tape.interactions) == 1
    interaction = tape.interactions[0]
    assert interaction.request.path == '/v1/messages'
    assert interaction.response_start.status == 200
    assert b''.join(chunk.data for chunk in interaction.chunks) == b''.join(chunks)
    assert interaction.response_end.outcome == 'eof'


def test_incomplete_tape_is_rejected(tmp_path):
    path = tmp_path / 'partial.llmtape'
    path.write_text(
        json.dumps(
            {
                'kind': 'tape_start',
                'version': 1,
                'created_at': '2026-08-03T00:00:00+00:00',
            }
        )
        + '\n',
        encoding='utf-8',
    )

    with pytest.raises(TapeFormatError, match='missing tape_end'):
        load_tape(path)


async def test_tape_refuses_to_overwrite_existing_file(tmp_path):
    path = tmp_path / 'existing.llmtape'
    path.write_text('keep me', encoding='utf-8')

    with pytest.raises(FileExistsError):
        TapeWriter(path)

    assert path.read_text(encoding='utf-8') == 'keep me'
