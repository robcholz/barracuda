from __future__ import annotations

from collections.abc import Iterator

import pytest
from loguru import logger


@pytest.fixture
def log_messages() -> Iterator[list[str]]:
    messages: list[str] = []
    handler_id = logger.add(
        lambda message: messages.append(str(message)),
        level='DEBUG',
        format='{message}',
    )
    try:
        yield messages
    finally:
        logger.remove(handler_id)
