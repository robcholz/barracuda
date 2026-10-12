"""Conversation keys that let replay serve concurrent callers correctly.

Replay serves one recorded response per request. When several conversations
call the model at the same time, for example a root agent and its subagents,
their requests can reach the server in a different order than when the tape
was recorded. Each request is therefore tagged with a key derived from its
first user message, which is stable for one conversation and differs between
concurrent ones. Replay serves the earliest unconsumed interaction with the
same key, so order is kept within a conversation but not across them.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any


def conversation_key(body: bytes) -> str | None:
    """Key of the request's conversation, or None for non-chat bodies.

    Understands OpenAI-compatible and Anthropic Messages request bodies; the
    first user message's text (string content or text blocks) is hashed.
    """

    try:
        payload = json.loads(body)
    except (ValueError, UnicodeDecodeError):
        return None
    messages = payload.get('messages') if isinstance(payload, dict) else None
    if not isinstance(messages, list):
        return None
    for message in messages:
        if isinstance(message, dict) and message.get('role') == 'user':
            text = _text(message.get('content'))
            return hashlib.sha256(text.encode('utf-8')).hexdigest()[:16]
    return None


def _text(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return ''.join(
            block.get('text', '')
            for block in content
            if isinstance(block, dict) and isinstance(block.get('text'), str)
        )
    return ''
