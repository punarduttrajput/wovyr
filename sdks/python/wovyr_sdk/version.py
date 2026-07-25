"""DX-303: SDK ↔ server version awareness.

The SDK's version tracks the platform release it was written against
(`0.3.0` ↔ wovyr-server 0.3.0) — same major.minor means same API surface.
At runtime the version comes from package metadata when installed; the
fallback constant covers source-checkout use and is asserted against
`pyproject.toml` by the unit suite so drift can't ship."""

from __future__ import annotations

import re
from importlib import metadata
from typing import Optional

_FALLBACK_VERSION = "0.3.0"


def sdk_version() -> str:
    """The installed `wovyr-sdk` version, or the source-tree fallback."""
    try:
        return metadata.version("wovyr-sdk")
    except metadata.PackageNotFoundError:
        return _FALLBACK_VERSION


class WovyrVersionSkewWarning(UserWarning):
    """Emitted (once per client) when the server's major.minor differs from
    the release this SDK tracks. Filterable like any warning category:
    `warnings.simplefilter("ignore", WovyrVersionSkewWarning)`."""


def version_skew(sdk: str, server: str) -> Optional[str]:
    """Human-readable warning when `sdk` and `server` disagree on major.minor
    — `None` when they agree (patch-level differences are compatible by
    policy) or when either version is unparseable (a dev build like
    `"dry-run"` should not spam warnings)."""
    parsed_sdk = _parse(sdk)
    parsed_server = _parse(server)
    if parsed_sdk is None or parsed_server is None:
        return None
    if parsed_sdk == parsed_server:
        return None
    behind = "SDK" if parsed_sdk < parsed_server else "server"
    return (
        f"wovyr-sdk {sdk} was written against wovyr-server "
        f"{parsed_sdk[0]}.{parsed_sdk[1]}.x, but the server reports {server} — "
        f"routes and shapes may differ. Upgrade the {behind} to matching "
        f"major.minor."
    )


def _parse(v: str) -> Optional[tuple[int, int]]:
    m = re.match(r"^(\d+)\.(\d+)\.", v.strip())
    return (int(m.group(1)), int(m.group(2))) if m else None
