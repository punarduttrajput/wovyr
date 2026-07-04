"""Auto-iterates a cursor-paginated list endpoint, yielding one item at a time
and fetching the next page on demand."""

from __future__ import annotations

from typing import Any, Callable, Iterator

from .types import Page


def paginate_all(fetch_page: Callable[..., Page], **params: Any) -> Iterator[Any]:
    """Works with any resource method shaped like `(*, cursor=None, **params)
    -> Page`, e.g. `paginate_all(client.agents.list, limit=25)`. Stops once
    `has_more` is false or `next_cursor` is `None`, whichever comes first."""
    cursor = None
    while True:
        page = fetch_page(cursor=cursor, **params)
        for item in page["data"]:
            yield item
        if not page["has_more"] or not page["next_cursor"]:
            return
        cursor = page["next_cursor"]
