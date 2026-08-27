"""HTTP header filtering shared by recording and replay."""

from __future__ import annotations

from collections.abc import Iterable

from multidict import CIMultiDict

HeaderPairs = list[tuple[str, str]]

HOP_BY_HOP_HEADERS = frozenset(
    {
        'connection',
        'keep-alive',
        'proxy-authenticate',
        'proxy-authorization',
        'te',
        'trailer',
        'trailers',
        'transfer-encoding',
        'upgrade',
    }
)

SENSITIVE_REQUEST_HEADERS = frozenset(
    {
        'authorization',
        'cookie',
        'proxy-authorization',
        'x-api-key',
        'api-key',
        'x-auth-token',
        'x-amz-security-token',
    }
)


def decode_raw_headers(raw_headers: Iterable[tuple[bytes, bytes]]) -> HeaderPairs:
    """Decode raw HTTP headers without losing order or duplicate fields."""

    return [
        (name.decode('latin-1'), value.decode('latin-1')) for name, value in raw_headers
    ]


def stored_request_headers(raw_headers: Iterable[tuple[bytes, bytes]]) -> HeaderPairs:
    """Return non-hop-by-hop request headers with credentials redacted."""

    stored: HeaderPairs = []
    for name, value in decode_raw_headers(raw_headers):
        lower_name = name.lower()
        if lower_name in HOP_BY_HOP_HEADERS:
            continue
        if lower_name in SENSITIVE_REQUEST_HEADERS:
            stored.append((name, '***'))
        else:
            stored.append((name, value))
    return stored


def forwarded_request_headers(
    raw_headers: Iterable[tuple[bytes, bytes]],
    *,
    decoded_request_body: bool,
) -> CIMultiDict[str]:
    """Build headers for the upstream request.

    aiohttp's server side presents a decoded request body for common content
    encodings. In that case the original encoding and length no longer describe
    the forwarded bytes, so both fields are removed and aiohttp recomputes the
    length.
    """

    forwarded: CIMultiDict[str] = CIMultiDict()
    for name, value in decode_raw_headers(raw_headers):
        lower_name = name.lower()
        if lower_name in HOP_BY_HOP_HEADERS or lower_name in {
            'host',
            'content-length',
        }:
            continue
        if decoded_request_body and lower_name == 'content-encoding':
            continue
        forwarded.add(name, value)
    return forwarded


def response_headers(raw_headers: Iterable[tuple[bytes, bytes]]) -> HeaderPairs:
    """Return replayable end-to-end response headers."""

    return [
        (name, value)
        for name, value in decode_raw_headers(raw_headers)
        if name.lower() not in HOP_BY_HOP_HEADERS
    ]


def to_multidict(headers: Iterable[tuple[str, str]]) -> CIMultiDict[str]:
    """Convert stored header pairs to an aiohttp-compatible multidict."""

    result: CIMultiDict[str] = CIMultiDict()
    for name, value in headers:
        result.add(name, value)
    return result
