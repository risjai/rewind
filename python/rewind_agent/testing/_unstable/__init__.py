"""Explicitly unstable testing helpers.

Symbols under :mod:`rewind_agent.testing._unstable` are NOT covered by
the SDK's semver commitment. They may be renamed, restructured, or
removed in any 0.x release without warning. Use them only inside the
SDK's own tests, or accept that downstream tests pinning to them will
break on upgrade.

If a downstream consumer needs a stable version of something in here,
file an issue or a PR — promotion to :mod:`rewind_agent.testing` is a
deliberate decision, not an accident.

This package is intentionally empty in v0.17 — it exists as the boundary
marker so the stability commitment of the parent module is unambiguous.
"""

__all__: list[str] = []
