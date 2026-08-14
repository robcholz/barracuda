from __future__ import annotations

from io import StringIO

from loguru import logger

from llm_tape.cli import LOG_FORMAT


class _TerminalBuffer(StringIO):
    def isatty(self) -> bool:
        return True


def test_loguru_colors_only_interactive_output():
    terminal = _TerminalBuffer()
    plain_file = StringIO()
    terminal_handler = logger.add(
        terminal,
        level='INFO',
        format=LOG_FORMAT,
        colorize=None,
    )
    plain_handler = logger.add(
        plain_file,
        level='INFO',
        format=LOG_FORMAT,
        colorize=None,
    )
    try:
        logger.info('color probe')
    finally:
        logger.remove(terminal_handler)
        logger.remove(plain_handler)

    assert '\x1b[' in terminal.getvalue()
    assert '\x1b[' not in plain_file.getvalue()
