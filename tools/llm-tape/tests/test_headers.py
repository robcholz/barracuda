from __future__ import annotations

from llm_tape.headers import forwarded_request_headers


def test_a_client_without_accept_encoding_gets_identity_upstream():
    forwarded = forwarded_request_headers(
        [(b'Content-Type', b'application/json')], decoded_request_body=False
    )
    assert forwarded['Accept-Encoding'] == 'identity'


def test_a_client_accept_encoding_is_forwarded_unchanged():
    forwarded = forwarded_request_headers(
        [(b'accept-encoding', b'gzip')], decoded_request_body=False
    )
    assert forwarded.getall('Accept-Encoding') == ['gzip']
