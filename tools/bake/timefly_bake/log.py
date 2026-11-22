"""Logging for the bake tool (spec §20).

``configure`` sends human-readable records to stderr and, when given an output directory, a
DEBUG-level ``bake.log`` there. ``@traced`` is the tracing aspect: every public function in the
package wears it, and ``tests/test_log.py`` enforces that.
"""

from __future__ import annotations

import functools
import inspect
import logging
import os
import sys
import time
from pathlib import Path
from typing import Any, Callable, TypeVar

F = TypeVar("F", bound=Callable[..., Any])

LOGGER_NAME = "timefly_bake"
_TRACE_LOGGER = logging.getLogger(f"{LOGGER_NAME}.trace")
_MAX_TEXT = 200
_MAX_ITEMS = 5
_HANDLER_TAG = "_timefly_bake_handler"


class _RedactingFormatter(logging.Formatter):
    """Formats a record, traceback included, with the home directory replaced by ``~``."""

    def format(self, record: logging.LogRecord) -> str:
        return redact(super().format(record))


def configure(out_dir: Path | None, level: str = "INFO") -> None:
    """Route ``timefly_bake`` records to stderr at ``level`` and to ``out_dir/bake.log`` at DEBUG.

    Calling it again replaces the handlers it installed before; ``configure(None)`` keeps only
    stderr.
    """
    logger = logging.getLogger(LOGGER_NAME)
    for handler in [h for h in logger.handlers if getattr(h, _HANDLER_TAG, False)]:
        logger.removeHandler(handler)
        handler.close()
    logger.setLevel(logging.DEBUG)

    stderr = logging.StreamHandler(sys.stderr)
    stderr.setLevel(level.upper())
    stderr.setFormatter(_RedactingFormatter("%(levelname)-7s %(message)s"))
    setattr(stderr, _HANDLER_TAG, True)
    logger.addHandler(stderr)

    if out_dir is not None:
        out_dir = Path(out_dir)
        out_dir.mkdir(parents=True, exist_ok=True)
        file_handler = logging.FileHandler(out_dir / "bake.log", encoding="utf-8")
        file_handler.setLevel(logging.DEBUG)
        file_handler.setFormatter(
            _RedactingFormatter("%(asctime)s %(levelname)-7s %(name)s: %(message)s")
        )
        setattr(file_handler, _HANDLER_TAG, True)
        logger.addHandler(file_handler)


def _home_forms() -> list[str]:
    forms = set()
    home = os.path.expanduser("~")
    if home and home not in ("/", "~"):
        forms.add(home.rstrip("/\\"))
        try:
            forms.add(str(Path(home).resolve()).rstrip("/\\"))
        except OSError:
            pass
    return sorted((f for f in forms if f), key=len, reverse=True)


def redact(text: str) -> str:
    """Replace the home directory (raw and resolved) with ``~``."""
    for form in _home_forms():
        text = text.replace(form, "~")
    return text


def _cut(text: str) -> str:
    if len(text) <= _MAX_TEXT:
        return text
    return f"{text[:_MAX_TEXT]}...(+{len(text) - _MAX_TEXT} chars)"


def summarize(value: Any) -> str:
    """A short, value-free-for-arrays description of ``value`` for the log."""
    if hasattr(value, "shape") and hasattr(value, "dtype"):
        return f"{type(value).__name__}(shape={tuple(value.shape)}, dtype={value.dtype})"
    if isinstance(value, os.PathLike):
        return _cut(redact(os.fspath(value)))
    if isinstance(value, str):
        return repr(_cut(redact(value)))
    if isinstance(value, (bytes, bytearray, memoryview)):
        return f"{type(value).__name__}(len={len(value)})"
    if isinstance(value, (list, tuple)):
        if len(value) <= _MAX_ITEMS:
            inner = ", ".join(summarize(v) for v in value)
            return f"[{inner}]" if isinstance(value, list) else f"({inner})"
        return f"{type(value).__name__}(len={len(value)})"
    if isinstance(value, (dict, set, frozenset)):
        return f"{type(value).__name__}(len={len(value)})"
    return _cut(redact(repr(value)))


def _describe_args(signature: inspect.Signature | None, args: tuple, kwargs: dict) -> str:
    if signature is not None:
        try:
            bound = signature.bind_partial(*args, **kwargs)
            return ", ".join(f"{name}={summarize(v)}" for name, v in bound.arguments.items())
        except TypeError:
            pass
    parts = [summarize(v) for v in args] + [f"{k}={summarize(v)}" for k, v in kwargs.items()]
    return ", ".join(parts)


def traced(func: F) -> F:
    """Log entry (summarized arguments), exit (duration and result) and exceptions."""
    name = f"{func.__module__}.{func.__qualname__}"
    try:
        signature: inspect.Signature | None = inspect.signature(func)
    except (TypeError, ValueError):
        signature = None

    @functools.wraps(func)
    def wrapper(*args: Any, **kwargs: Any) -> Any:
        logger = _TRACE_LOGGER
        if logger.isEnabledFor(logging.DEBUG):
            logger.debug("-> %s(%s)", name, _describe_args(signature, args, kwargs))
        start = time.perf_counter()
        try:
            result = func(*args, **kwargs)
        except BaseException as exc:
            ms = (time.perf_counter() - start) * 1000.0
            logger.error(
                "!! %s failed after %.1f ms: %s",
                name,
                ms,
                redact(f"{type(exc).__name__}: {exc}"),
                exc_info=True,
            )
            raise
        if logger.isEnabledFor(logging.DEBUG):
            ms = (time.perf_counter() - start) * 1000.0
            logger.debug("<- %s %.1f ms -> %s", name, ms, summarize(result))
        return result

    wrapper.__traced__ = True  # type: ignore[attr-defined]
    return wrapper  # type: ignore[return-value]
