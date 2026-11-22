import importlib
import inspect
import logging
import pkgutil
from pathlib import Path

import numpy as np
import pytest

import timefly_bake
from timefly_bake import log as bake_log
from timefly_bake.log import traced


@traced
def add(a, b=1):
    return a + b


@traced
def mean_of(values):
    return float(values.mean())


@traced
def explode(path):
    raise ValueError("bad input")


@traced
def echo(value):
    return value


def records_of(caplog, name):
    return [r for r in caplog.records if name in r.getMessage()]


def test_entry_exit_and_duration(caplog):
    caplog.set_level(logging.DEBUG, logger="timefly_bake")
    assert add(2, b=3) == 5
    messages = [r.getMessage() for r in caplog.records]
    entry = [m for m in messages if m.startswith("-> ") and "add" in m]
    exit_ = [m for m in messages if m.startswith("<- ") and "add" in m]
    assert entry and "a=2" in entry[0] and "b=3" in entry[0]
    assert exit_ and " ms" in exit_[0] and "-> 5" in exit_[0]


def test_ndarray_summarized_as_shape_and_dtype(caplog):
    caplog.set_level(logging.DEBUG, logger="timefly_bake")
    values = np.arange(12, dtype=np.float32).reshape(3, 4) + 1234.5
    mean_of(values)
    text = "\n".join(r.getMessage() for r in caplog.records)
    assert "ndarray(shape=(3, 4), dtype=float32)" in text
    assert "1234.5" not in text and "1235.5" not in text


def test_exception_logged_with_traceback_and_reraised(caplog):
    caplog.set_level(logging.DEBUG, logger="timefly_bake")
    with pytest.raises(ValueError, match="bad input"):
        explode(Path.home() / "secret" / "file.nc")
    errors = [r for r in caplog.records if r.levelno >= logging.ERROR]
    assert errors, "no error record"
    assert errors[0].exc_info is not None
    assert "explode" in errors[0].getMessage()
    text = "\n".join(r.getMessage() for r in caplog.records)
    assert str(Path.home()) not in text
    assert "~/secret/file.nc" in text


def test_return_value_unchanged():
    marker = object()
    assert echo(marker) is marker
    big = "x" * 10_000
    assert echo(big) == big


def test_long_strings_are_cut(caplog):
    caplog.set_level(logging.DEBUG, logger="timefly_bake")
    echo("y" * 10_000)
    text = "\n".join(r.getMessage() for r in caplog.records)
    assert "y" * 10_000 not in text
    assert "yyyy" in text


def test_wraps_metadata_and_marker():
    @traced
    def documented(x):
        """Doc."""
        return x

    assert documented.__name__ == "documented"
    assert documented.__doc__ == "Doc."
    assert documented.__traced__ is True
    assert inspect.signature(documented) == inspect.signature(documented.__wrapped__)


def test_configure_writes_bake_log(tmp_path):
    bake_log.configure(tmp_path, level="INFO")
    try:
        logger = logging.getLogger("timefly_bake.test")
        logger.debug("debug detail %s", 42)
        logger.info("visible")
        for handler in logging.getLogger("timefly_bake").handlers:
            handler.flush()
        text = (tmp_path / "bake.log").read_text()
        assert "debug detail 42" in text
        assert "visible" in text
    finally:
        bake_log.configure(None)


def test_every_public_function_is_traced():
    missing = []
    for info in pkgutil.walk_packages(timefly_bake.__path__, prefix="timefly_bake."):
        if info.name == "timefly_bake.log":
            continue
        module = importlib.import_module(info.name)
        for name, obj in vars(module).items():
            if name.startswith("_") or not inspect.isfunction(obj):
                continue
            if getattr(obj, "__module__", None) != module.__name__:
                continue
            if not getattr(obj, "__traced__", False):
                missing.append(f"{module.__name__}.{name}")
    assert not missing, f"public functions without @traced: {missing}"
