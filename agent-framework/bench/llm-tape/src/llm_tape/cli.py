"""Command-line interface for recording and replaying LLM byte tapes."""

from __future__ import annotations

import argparse
from collections.abc import Sequence
from pathlib import Path

from aiohttp import web

from .recorder import create_recorder_app
from .replay import create_replay_app
from .tape import TapeFormatError

DEFAULT_LISTEN = '127.0.0.1:8787'


def build_parser() -> argparse.ArgumentParser:
    """Build the public CLI parser."""

    parser = argparse.ArgumentParser(
        prog='llm-tape',
        description='Record and replay uninterpreted LLM HTTP response bytes.',
    )
    subparsers = parser.add_subparsers(dest='command', required=True)

    record = subparsers.add_parser('record', help='record through a reverse proxy')
    record.add_argument('--listen', default=DEFAULT_LISTEN, metavar='HOST:PORT')
    record.add_argument('--upstream', required=True, metavar='URL')
    record.add_argument('--output', required=True, type=Path, metavar='TAPE')

    replay = subparsers.add_parser(
        'replay', help='serve a tape with its original timing'
    )
    replay.add_argument('--listen', default=DEFAULT_LISTEN, metavar='HOST:PORT')
    replay.add_argument('tape', type=Path, metavar='TAPE')
    return parser


def main(argv: Sequence[str] | None = None) -> None:
    """Run the requested recorder or replay server."""

    args = build_parser().parse_args(argv)
    try:
        host, port = _parse_listen(args.listen)
        if args.command == 'record':
            app = create_recorder_app(upstream=args.upstream, output=args.output)
            detail = f'upstream={args.upstream} output={args.output}'
        else:
            app = create_replay_app(args.tape)
            detail = f'tape={args.tape}'
    except (FileExistsError, OSError, TapeFormatError, ValueError) as exc:
        raise SystemExit(f'llm-tape: {exc}') from exc

    print(f'llm-tape {args.command}: http://{host}:{port} ({detail})')
    web.run_app(app, host=host, port=port, print=None)


def _parse_listen(value: str) -> tuple[str, int]:
    try:
        host, raw_port = value.rsplit(':', 1)
        port = int(raw_port)
    except (ValueError, TypeError) as exc:
        raise ValueError('listen must use HOST:PORT') from exc
    if not host:
        raise ValueError('listen host cannot be empty')
    if not 1 <= port <= 65535:
        raise ValueError('listen port must be between 1 and 65535')
    return host, port
