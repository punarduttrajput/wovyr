"""Minimal Server-Sent Events line parser — only what `agents:stream` actually
emits (no `id:`/`retry:` support, no reconnection): named events split
unnamed `data:` frames from the terminal `result`/`error` one."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Iterable, Iterator


@dataclass
class SseFrame:
    """One parsed frame. `event` defaults to `"message"` per the SSE spec when
    the server omits it (as `agents:stream`'s `data:`-only frames do); `data`
    is the concatenation of every `data:` line, newline-joined."""

    event: str
    data: str


def parse_sse(lines: Iterable[bytes]) -> Iterator[SseFrame]:
    """Parse a stream of raw response lines (as yielded by
    `RawResponse.iter_lines`) into frames, one per blank-line-terminated
    block."""
    event = "message"
    data_lines: list[str] = []

    def flush() -> Iterator[SseFrame]:
        nonlocal event, data_lines
        if data_lines:
            yield SseFrame(event=event, data="\n".join(data_lines))
        event = "message"
        data_lines = []

    for raw_line in lines:
        line = raw_line.decode("utf-8").rstrip("\r\n")
        if line == "":
            yield from flush()
            continue
        if line.startswith("event:"):
            event = line[len("event:") :].strip()
        elif line.startswith("data:"):
            data_lines.append(line[len("data:") :].lstrip())
    yield from flush()
