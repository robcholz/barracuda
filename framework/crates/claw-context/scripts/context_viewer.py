#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "rich>=13.9,<15",
#   "textual>=1,<8",
#   "websockets>=14,<17",
# ]
# ///
"""Live terminal viewer for intrusive claw-context snapshots."""

from __future__ import annotations

import argparse
import asyncio
import json
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

from rich.console import Group
from rich.panel import Panel
from rich.table import Table
from rich.text import Text
from textual.app import App, ComposeResult
from textual.containers import VerticalScroll
from textual.widgets import Footer, Static
from websockets.asyncio.client import connect
from websockets.exceptions import ConnectionClosed

DEFAULT_URI = "ws://127.0.0.1:9464/ws/context"


def full_text(value: Any) -> Text:
    if isinstance(value, str):
        rendered = value
    else:
        rendered = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    return Text(rendered)


def kind_name(kind: Any) -> str:
    if not isinstance(kind, dict):
        return "unknown"
    name = str(kind.get("name", "unknown"))
    label = kind.get("label")
    return f"{name}:{label}" if label else name


def encoded_bytes(value: Any) -> int:
    return len(
        json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    )


def render_snapshot(snapshot: dict[str, Any], status: str) -> Group:
    system = snapshot.get("system") if isinstance(snapshot.get("system"), dict) else {}
    history = (
        snapshot.get("history") if isinstance(snapshot.get("history"), dict) else {}
    )
    reminders = (
        snapshot.get("reminders")
        if isinstance(snapshot.get("reminders"), dict)
        else {}
    )
    blocks = system.get("blocks") if isinstance(system.get("blocks"), list) else []
    messages = history.get("value") if isinstance(history.get("value"), list) else []
    rendered_reminders = (
        reminders.get("rendered")
        if isinstance(reminders.get("rendered"), list)
        else []
    )
    rendered_system = (
        system.get("rendered") if isinstance(system.get("rendered"), str) else ""
    )

    summary = Table.grid(expand=True)
    summary.add_column()
    summary.add_column(justify="right")
    summary.add_row(
        (
            f"[bold]Snapshot {snapshot.get('sequence', '?')}[/bold]  "
            f"content_version={snapshot.get('content_version', '?')}"
        ),
        (
            f"system={system.get('bytes', 0)} B  "
            f"history={encoded_bytes(messages)} B  "
            f"blocks={len(blocks)}  messages={len(messages)}"
        ),
    )

    block_table = Table(expand=True, show_lines=True)
    block_table.add_column("Band", width=9, no_wrap=True)
    block_table.add_column("Scope", width=13, no_wrap=True)
    block_table.add_column("Kind", width=23)
    block_table.add_column("Bytes", justify="right", width=7)
    block_table.add_column("Raw content", ratio=1)
    for block in blocks:
        if not isinstance(block, dict):
            continue
        kind = block.get("kind") if isinstance(block.get("kind"), dict) else {}
        block_table.add_row(
            str(kind.get("band", "?")),
            str(kind.get("scope", "?")),
            kind_name(kind),
            str(block.get("bytes", "?")),
            full_text(block.get("content", "")),
        )

    history_table = Table(expand=True, show_lines=True)
    history_table.add_column("#", justify="right", width=4)
    history_table.add_column("Role", width=11, no_wrap=True)
    history_table.add_column("Message", ratio=1)
    for index, message in enumerate(messages):
        role = message.get("role", "?") if isinstance(message, dict) else "?"
        content = message.get("content", message) if isinstance(message, dict) else message
        history_table.add_row(
            str(index),
            str(role),
            full_text(content),
        )

    reminder_table = Table(expand=True, show_lines=True)
    reminder_table.add_column("#", justify="right", width=4)
    reminder_table.add_column("Role", width=11, no_wrap=True)
    reminder_table.add_column("Rendered reminder", ratio=1)
    for index, reminder in enumerate(rendered_reminders):
        role = reminder.get("role", "?") if isinstance(reminder, dict) else "?"
        content = (
            reminder.get("content", reminder)
            if isinstance(reminder, dict)
            else reminder
        )
        reminder_table.add_row(
            str(index),
            str(role),
            full_text(content),
        )

    return Group(
        Panel(summary, title=status, border_style="cyan"),
        Panel(block_table, title="System blocks"),
        Panel(history_table, title="History"),
        Panel(reminder_table, title="Reminders"),
        Panel(full_text(rendered_system), title="Rendered system prompt"),
    )


def initial_view(uri: str) -> Panel:
    return Panel(
        Text(f"Connecting to {uri}\nWaiting for Context::request() snapshots…"),
        title="claw-context",
        border_style="yellow",
    )


def append_snapshot(path: Path, snapshot: dict[str, Any]) -> None:
    with path.open("a", encoding="utf-8") as output:
        json.dump(snapshot, output, ensure_ascii=False, separators=(",", ":"))
        output.write("\n")


class ContextViewer(App[int]):
    """Scrollable live dashboard for Context snapshots."""

    CSS = """
    Screen {
        layout: vertical;
    }

    #viewport {
        width: 100%;
        height: 1fr;
        overflow-x: hidden;
        overflow-y: auto;
        scrollbar-gutter: stable;
    }

    #content {
        width: 100%;
        height: auto;
    }

    Footer {
        dock: bottom;
    }
    """

    BINDINGS = [
        ("q", "quit", "Quit"),
        ("ctrl+c", "quit", "Quit"),
    ]

    def __init__(self, args: argparse.Namespace) -> None:
        super().__init__()
        self.args = args
        self.latest: dict[str, Any] | None = None

    def compose(self) -> ComposeResult:
        with VerticalScroll(id="viewport"):
            yield Static(initial_view(self.args.uri), id="content")
        yield Footer()

    def on_mount(self) -> None:
        self.query_one("#viewport", VerticalScroll).focus()
        self.run_worker(self.watch(), name="context-websocket", exclusive=True)

    def update_view(self, status: str) -> None:
        content = self.query_one("#content", Static)
        if self.latest is None:
            content.update(Panel(status, title="claw-context", border_style="yellow"))
        else:
            content.update(render_snapshot(self.latest, status))

    async def watch(self) -> None:
        while True:
            try:
                async with connect(self.args.uri, max_size=None) as websocket:
                    status = f"connected · {self.args.uri}"
                    async for payload in websocket:
                        try:
                            event = json.loads(payload)
                        except json.JSONDecodeError:
                            self.update_view("ignored malformed JSON frame")
                            continue
                        if not isinstance(event, dict):
                            self.update_view("ignored non-object frame")
                            continue

                        event_type = event.get("event")
                        if event_type == "context_snapshot":
                            self.latest = event
                            if self.args.output is not None:
                                append_snapshot(self.args.output, event)
                            self.update_view(status)
                            if self.args.once:
                                self.exit(result=0)
                                return
                        elif event_type == "gap":
                            self.update_view(
                                f"stream gap · skipped {event.get('skipped', '?')} snapshots"
                            )
                        elif event_type == "ready":
                            self.update_view(
                                f"connected · {self.args.uri} · waiting for request"
                            )
            except (ConnectionClosed, OSError, TimeoutError) as error:
                self.update_view(f"disconnected · {error}")

            if not self.args.reconnect:
                self.exit(result=1)
                return
            await asyncio.sleep(self.args.reconnect_delay)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render claw-context WebSocket snapshots in a live terminal dashboard."
    )
    parser.add_argument("--uri", default=DEFAULT_URI, help=f"WebSocket URI ({DEFAULT_URI})")
    parser.add_argument(
        "--output",
        type=Path,
        help="append every context_snapshot to this JSONL file",
    )
    parser.add_argument("--once", action="store_true", help="exit after the first snapshot")
    parser.add_argument(
        "--no-reconnect",
        dest="reconnect",
        action="store_false",
        help="exit when the WebSocket disconnects",
    )
    parser.add_argument(
        "--reconnect-delay",
        type=float,
        default=1.0,
        help="seconds between reconnect attempts (default: 1.0)",
    )
    args = parser.parse_args()
    parsed_uri = urlparse(args.uri)
    if parsed_uri.scheme not in {"ws", "wss"} or not parsed_uri.netloc:
        parser.error("--uri must be an absolute ws:// or wss:// URI")
    return args


def main() -> int:
    args = parse_args()
    result = ContextViewer(args).run()
    return 0 if result is None else result


if __name__ == "__main__":
    raise SystemExit(main())
