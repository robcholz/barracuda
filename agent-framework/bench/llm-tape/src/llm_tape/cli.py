"""Command-line interface for recording and replaying LLM byte tapes."""

from __future__ import annotations

import argparse
import sys
from collections.abc import Sequence
from pathlib import Path

from aiohttp import web
from loguru import logger

from .recorder import create_recorder_app
from .replay import create_replay_app
from .tape import TapeFormatError

DEFAULT_LISTEN = '127.0.0.1:8787'
LOG_FORMAT = (
    '<green>{time:YYYY-MM-DD HH:mm:ss.SSS}</green> | '
    '<level>{level: <8}</level> | '
    '<cyan>{name}</cyan> - <level>{message}</level>'
)


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
    _add_log_level_argument(record)

    replay = subparsers.add_parser(
        'replay', help='serve a tape with its original timing'
    )
    replay.add_argument('--listen', default=DEFAULT_LISTEN, metavar='HOST:PORT')
    replay.add_argument('tape', type=Path, metavar='TAPE')
    _add_log_level_argument(replay)
    return parser


def main(argv: Sequence[str] | None = None) -> None:
    """Run the requested recorder or replay server."""

    args = build_parser().parse_args(argv)
    _configure_logging(args.log_level)
    try:
        host, port = _parse_listen(args.listen)
        if args.command == 'record':
            app = create_recorder_app(upstream=args.upstream, output=args.output)
            detail = f'upstream={args.upstream} output={args.output}'
        else:
            app = create_replay_app(args.tape)
            detail = f'tape={args.tape}'
    except (FileExistsError, OSError, TapeFormatError, ValueError) as exc:
        logger.error('startup failed error={}', exc)
        raise SystemExit(f'llm-tape: {exc}') from exc

    logger.info(
        'server starting mode={} listen=http://{}:{} {}',
        args.command,
        host,
        port,
        detail,
    )
    try:
        web.run_app(
            app,
            host=host,
            port=port,
            print=None,
            access_log=None,
        )
    finally:
        logger.info('server stopped mode={}', args.command)


def _configure_logging(level: str) -> None:
    logger.remove()
    logger.add(
        sys.stderr,
        level=level,
        format=LOG_FORMAT,
        colorize=None,
        backtrace=False,
        diagnose=False,
    )


def _add_log_level_argument(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        '--log-level',
        default='INFO',
        choices=('DEBUG', 'INFO', 'WARNING', 'ERROR'),
        type=str.upper,
        help='logging verbosity (default: INFO)',
    )


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
