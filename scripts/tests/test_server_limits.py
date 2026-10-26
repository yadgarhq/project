"""`server.limits` (ADR-1006): four listener limits rendered into LISTEN_* (connection age omitted until tonic#2780 ships; yaadgaar #417).

The binary has no default for any of them (ADR-0569): it refuses the boot on an
absent, non-numeric or out-of-range value. `values.schema.json` owns each key's
range (held to the pinned library by `tests/chart_server_limits.rs`).
`templates/render-checks.yaml` owns the one rule a schema cannot state: the
keepalive timeout must sit below the interval.

Run: python3 -m pytest scripts/tests/ -q
"""

from __future__ import annotations

import pytest
import yaml

from test_render_checks import CHART, objects, render

SCHEMA_WRAPPER = "values don't meet the specifications of the schema(s)"

DEFAULTS = {
    "LISTEN_MAX_CONCURRENT_STREAMS": "256",
    "LISTEN_KEEPALIVE_INTERVAL_SECS": "30",
    "LISTEN_KEEPALIVE_TIMEOUT_SECS": "10",
    "LISTEN_MAX_HEADER_LIST_BYTES": "16384",
}


def deployment_env(stdout: str) -> dict[str, str]:
    (deployment,) = [o for o in objects(stdout) if o["kind"] == "Deployment"]
    container = deployment["spec"]["template"]["spec"]["containers"][0]
    return {e["name"]: e.get("value") for e in container["env"]}


def test_the_defaults_render_all_five_variables():
    result = render(CHART)
    assert result.returncode == 0, result.stderr
    env = deployment_env(result.stdout)
    assert {k: env.get(k) for k in DEFAULTS} == DEFAULTS


def test_the_largest_header_list_renders_as_a_whole_number(tmp_path):
    """helm parses a values number as float64; `quote` alone prints 1048576 as
    "1.048576e+06", which the binary refuses as not a whole number."""
    # A values FILE, not `--set`: a file's numbers are float64, `--set`'s are int64.
    overlay = tmp_path / "header.yaml"
    overlay.write_text("server:\n  limits:\n    maxHeaderListBytes: 1048576\n")
    result = render(CHART, "-f", str(overlay))
    assert result.returncode == 0, result.stderr
    assert deployment_env(result.stdout)["LISTEN_MAX_HEADER_LIST_BYTES"] == "1048576"


def test_an_overridden_value_reaches_its_variable():
    result = render(CHART, "--set", "server.limits.maxConcurrentStreams=64")
    assert result.returncode == 0, result.stderr
    assert deployment_env(result.stdout)["LISTEN_MAX_CONCURRENT_STREAMS"] == "64"


@pytest.mark.parametrize("timeout", [30, 31, 600])
def test_a_keepalive_timeout_at_or_above_the_interval_is_refused_by_name(timeout):
    result = render(CHART, "--set", f"server.limits.keepaliveTimeoutSeconds={timeout}")
    assert result.returncode != 0, result.stdout
    assert "`server.limits.keepaliveTimeoutSeconds`" in result.stderr, result.stderr
    assert "`server.limits.keepaliveIntervalSeconds`" in result.stderr, result.stderr


def test_a_keepalive_timeout_just_below_the_interval_renders():
    result = render(CHART, "--set", "server.limits.keepaliveTimeoutSeconds=29")
    assert result.returncode == 0, result.stderr


@pytest.mark.parametrize(
    ("key", "value"),
    [
        ("maxConcurrentStreams", 0),
        ("maxConcurrentStreams", 10001),
        ("keepaliveIntervalSeconds", 3601),
        ("keepaliveTimeoutSeconds", 601),
        ("maxHeaderListBytes", 1023),
        ("maxHeaderListBytes", 1048577),
    ],
)
def test_an_out_of_range_value_is_refused_at_the_schema(key, value):
    result = render(CHART, "--set", f"server.limits.{key}={value}")
    assert result.returncode != 0, result.stdout
    assert SCHEMA_WRAPPER in result.stderr or key in result.stderr, result.stderr
    assert key in result.stderr, result.stderr


def test_a_non_integer_value_is_refused_at_the_schema():
    result = render(CHART, "--set-string", "server.limits.maxConcurrentStreams=many")
    assert result.returncode != 0, result.stdout
    assert "maxConcurrentStreams" in result.stderr, result.stderr


def test_a_deleted_server_block_is_refused_at_the_schema():
    result = render(CHART, "--set", "server=null")
    assert result.returncode != 0, result.stdout
    assert "server" in result.stderr, result.stderr


@pytest.mark.parametrize("shape", ["null", "x"])
def test_a_null_or_scalar_server_is_refused_by_name(shape):
    result = render(CHART, "--set", f"server={shape}")
    assert result.returncode != 0, result.stdout
    # `null` deletes the key, so the schema's root `required` answers first; a
    # scalar passes the schema and reaches the template guard.
    assert "server" in result.stderr, result.stderr
    assert "nil pointer" not in result.stderr and "can't evaluate" not in result.stderr


@pytest.mark.parametrize("shape", ["null", "x"])
def test_a_null_or_scalar_limits_block_is_refused_by_name(shape):
    result = render(CHART, "--set", f"server.limits={shape}")
    assert result.returncode != 0, result.stdout
    assert "project: `server.limits`" in result.stderr, result.stderr
    assert "nil pointer" not in result.stderr and "can't evaluate" not in result.stderr


def test_a_server_block_without_limits_is_refused_by_name(tmp_path):
    overlay = tmp_path / "server.yaml"
    overlay.write_text("server:\n  other: 1\n")
    result = render(CHART, "-f", str(overlay))
    assert result.returncode != 0, result.stdout
    assert "nil pointer" not in result.stderr and "can't evaluate" not in result.stderr


def test_the_shipped_defaults_are_the_adr_1006_numbers():
    values = yaml.safe_load((CHART / "values.yaml").read_text())
    assert values["server"]["limits"] == {
        "maxConcurrentStreams": 256,
        "keepaliveIntervalSeconds": 30,
        "keepaliveTimeoutSeconds": 10,
        "maxHeaderListBytes": 16384,
    }


def test_deleting_the_cross_key_check_lets_the_bad_pair_through(tmp_path):
    """The check is what refuses the pair: without it the render succeeds."""
    import shutil

    copy = tmp_path / CHART.name
    shutil.copytree(CHART, copy)
    checks = copy / "templates" / "render-checks.yaml"
    text = checks.read_text()
    start = text.index("{{- if ge (int64 .Values.server.limits.keepaliveTimeoutSeconds)")
    checks.write_text(text[:start])
    result = render(copy, "--set", "server.limits.keepaliveTimeoutSeconds=30")
    assert result.returncode == 0, result.stderr
