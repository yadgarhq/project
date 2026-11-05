"""NUMERIC KNOBS RENDER AS WHOLE NUMBERS, OR THE RENDER REFUSES NAMING THE KEY (yaadgaar 482, 485).

A values FILE decodes every number as float64, and `quote` prints a float64 with `%v`:
1000000 becomes "1e+06". The binary parses neither. `int64` repairs the rendering but is
silent on a value that does not fit (1e20 becomes -9223372036854775808) and reads a
string base 0 ("0123" is 83), so every numeric knob is checked BEFORE the cast
(`templates/render-checks.yaml`): a string is at most 18 digits with no leading zero, a
number is whole and below 1e18, and the refusal names the key.

`replicaCount` 0 stays legal while autoscaling is off (ruled 2026-11-03), and
`test_zero_replicas_still_render` pins it. `autoscaling.minReplicas` below 2 is refused
while autoscaling is on (D55, yaadgaar 482), pinned at the end of this file.
`autoscaling.threshold` is left alone on purpose: KEDA reads "1e+06" as a float64.
`rollingUpdate.*` is an open map written out with `toYaml` and takes int-or-string
(`"25%"`), so it has no single whole-number rule to enforce here.

Mirrors yadgarhq/iam#150 (`scripts/tests/test_numeric_knobs.py`).

Run: python3 -m pytest scripts/tests/ -q
"""

from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path

import pytest
import yaml

REPO = Path(__file__).resolve().parents[2]
CHART = REPO / "chart"
CI_VALUES = CHART / "ci" / "values.yaml"
KEDA = ("--api-versions", "keda.sh/v1alpha1")
INT32_MAX = 2147483647
NINETEEN_NINES = "9" * 19
EIGHTEEN_NINES = "9" * 18


class QuotingDumper(yaml.SafeDumper):
    """Every string is written QUOTED. PyYAML leaves `1e3` and `08` bare, and helm's
    parser then reads them as the numbers 1000 and 8, so a case meant to hand the chart
    a string would hand it a number."""


QuotingDumper.add_representer(
    str, lambda dumper, data: dumper.represent_scalar("tag:yaml.org,2002:str", data, style='"')
)


def helm(*arguments: str) -> subprocess.CompletedProcess[str]:
    binary = shutil.which("helm")
    # NOT A SKIP (ADR-0650): a pass reported without helm is a pass nobody earned.
    assert binary, "helm is not on PATH; install it rather than skipping"
    return subprocess.run([binary, *arguments], capture_output=True, text=True)


def overlay(path: str, value, *, autoscaling: bool = False) -> dict:
    body: dict = {"autoscaling": {"enabled": True}} if autoscaling else {}
    node = body
    *parents, leaf = path.split(".")
    for part in parents:
        node = node.setdefault(part, {})
    node[leaf] = value
    return body


def render(body: dict, tmp_path: Path, *extra: str):
    tmp_path.mkdir(parents=True, exist_ok=True)
    values = tmp_path / "values.yaml"
    values.write_text(yaml.dump(body, Dumper=QuotingDumper))
    return helm("template", "x", str(CHART), "-f", str(CI_VALUES), "-f", str(values), *extra)


def rendered(body: dict, tmp_path: Path, *extra: str) -> str:
    result = render(body, tmp_path, *extra)
    assert result.returncode == 0, result.stderr
    return result.stdout


def refused_naming(body: dict, key: str, tmp_path: Path, *extra: str) -> str:
    result = render(body, tmp_path, *extra)
    assert result.returncode != 0, result.stdout
    combined = result.stdout + result.stderr
    assert key in combined, combined
    return combined


def set_string(assignment: str, *extra: str) -> subprocess.CompletedProcess[str]:
    return helm("template", "x", str(CHART), "-f", str(CI_VALUES), "--set-string", assignment, *extra)


def objects(stdout: str) -> list[dict]:
    return [d for d in yaml.safe_load_all(stdout) if isinstance(d, dict) and d.get("apiVersion")]


def deployment(stdout: str) -> dict:
    (found,) = [o for o in objects(stdout) if o["kind"] == "Deployment"]
    return found


def env(stdout: str) -> dict[str, str]:
    (container,) = deployment(stdout)["spec"]["template"]["spec"]["containers"]
    return {e["name"]: e["value"] for e in container["env"] if "value" in e}


def spec(stdout: str) -> dict:
    return deployment(stdout)["spec"]["template"]["spec"]


BAD_WHOLE = [1.5, 0.5, -1, "-1", "1.5", "abc", "", "0123", "08", "1e3", NINETEEN_NINES, 1e18, 1e20,
             True, [], {}, None]
PORT_KNOBS = [("projectDb.port", "PROJECT_DB_PORT")]


@pytest.mark.parametrize("key,var", PORT_KNOBS)
def test_a_port_renders_both_ends_and_refuses_one_past_each(key, var, tmp_path):
    for value in (1, 65535, 50052.0, "443"):
        assert env(rendered(overlay(key, value), tmp_path))[var] == str(int(value))
    for value in (0, 65536, "0", "65536", 1000000, -1, 1.5, "abc"):
        refused_naming(overlay(key, value), key, tmp_path)


@pytest.mark.parametrize("key,var", PORT_KNOBS)
@pytest.mark.parametrize("value", BAD_WHOLE, ids=repr)
def test_a_port_that_is_not_a_whole_number_refuses_naming_the_key(key, var, value, tmp_path):
    refused_naming(overlay(key, value), key, tmp_path)


@pytest.mark.parametrize("key,var", PORT_KNOBS)
def test_a_port_given_by_set_string_is_checked_too(key, var):
    ok = set_string(f"{key}=65535")
    assert ok.returncode == 0, ok.stderr
    assert env(ok.stdout)[var] == "65535"
    for value in ("65536", NINETEEN_NINES, "0123", "1.5"):
        bad = set_string(f"{key}={value}")
        assert bad.returncode != 0 and key in bad.stdout + bad.stderr, value


def test_a_values_file_replica_count_renders_as_an_integer(tmp_path):
    stdout = rendered(overlay("replicaCount", 1000000), tmp_path)
    assert "replicas: 1000000\n" in stdout
    assert "1e+06" not in stdout


def test_the_replica_count_ceiling_is_the_int32_field_and_both_ends_are_exact(tmp_path):
    assert f"replicas: {INT32_MAX}\n" in rendered(overlay("replicaCount", INT32_MAX), tmp_path)
    refused_naming(overlay("replicaCount", INT32_MAX + 1), "replicaCount", tmp_path)
    refused_naming(overlay("replicaCount", str(INT32_MAX + 1)), "replicaCount", tmp_path)


def test_the_whole_number_ceiling_splits_eighteen_digits_from_nineteen(tmp_path):
    """Every knob here carries a tighter bound, so the 18-digit string passes the
    whole-number rule and is caught by the bound ("at most"), while the 19-digit string
    is caught by the whole-number rule itself. The pair pins both layers apart."""
    eighteen = refused_naming(overlay("replicaCount", EIGHTEEN_NINES), "replicaCount", tmp_path)
    assert "at most" in eighteen and "whole number" not in eighteen, eighteen
    nineteen = refused_naming(overlay("replicaCount", NINETEEN_NINES), "replicaCount", tmp_path)
    assert "whole number" in nineteen, nineteen
    assert "at most" in refused_naming(overlay("replicaCount", 10**17), "replicaCount", tmp_path)
    assert "whole number" in refused_naming(overlay("replicaCount", 10**18), "replicaCount", tmp_path)


@pytest.mark.parametrize("value", BAD_WHOLE, ids=repr)
def test_a_replica_count_that_is_not_whole_refuses_naming_the_key(value, tmp_path):
    refused_naming(overlay("replicaCount", value), "replicaCount", tmp_path)


def test_a_replica_count_given_by_set_string_is_checked_too():
    ok = set_string(f"replicaCount={INT32_MAX}")
    assert ok.returncode == 0, ok.stderr
    assert f"replicas: {INT32_MAX}\n" in ok.stdout
    for value in (str(INT32_MAX + 1), NINETEEN_NINES, "0123", "-1"):
        bad = set_string(f"replicaCount={value}")
        assert bad.returncode != 0 and "replicaCount" in bad.stdout + bad.stderr, value


def test_zero_replicas_still_render(tmp_path):
    """`replicaCount: 0` is legal (scale to zero, ruled 2026-11-03, yaadgaar 482). The KEDA
    floor is a separate rule, tested at the end of this file."""
    stdout = rendered(overlay("replicaCount", 0), tmp_path)
    assert "replicas: 0\n" in stdout


def test_the_autoscaling_counts_render_as_integers_and_bound_both_ends(tmp_path):
    body = overlay("autoscaling.minReplicas", 1000000, autoscaling=True)
    body["autoscaling"]["maxReplicas"] = 2000000
    stdout = rendered(body, tmp_path, *KEDA)
    assert "minReplicaCount: 1000000\n" in stdout
    assert "maxReplicaCount: 2000000\n" in stdout
    assert "1e+06" not in stdout and "2e+06" not in stdout
    for key in ("minReplicas", "maxReplicas"):
        top = overlay(f"autoscaling.{key}", INT32_MAX, autoscaling=True)
        top["autoscaling"]["maxReplicas"] = INT32_MAX  # 645: the ceiling may not sit below the floor
        assert rendered(top, tmp_path, *KEDA)
        over = overlay(f"autoscaling.{key}", INT32_MAX + 1, autoscaling=True)
        refused_naming(over, f"autoscaling.{key}", tmp_path, *KEDA)
        for value in (1.5, "abc", -1, "", NINETEEN_NINES, str(INT32_MAX + 1), "0123"):
            bad = overlay(f"autoscaling.{key}", value, autoscaling=True)
            refused_naming(bad, f"autoscaling.{key}", tmp_path, *KEDA)


def test_the_autoscaling_counts_given_by_set_string_are_checked_too():
    for key in ("minReplicas", "maxReplicas"):
        ok = set_string(f"autoscaling.{key}={INT32_MAX}", "--set", "autoscaling.enabled=true",
                        "--set", f"autoscaling.maxReplicas={INT32_MAX}", *KEDA)  # 645: the ceiling may not sit below the floor
        assert ok.returncode == 0, ok.stderr
        bad = set_string(f"autoscaling.{key}={INT32_MAX + 1}", "--set", "autoscaling.enabled=true", *KEDA)
        assert bad.returncode != 0 and f"autoscaling.{key}" in bad.stdout + bad.stderr


def test_the_autoscaling_counts_are_not_read_while_autoscaling_is_off(tmp_path):
    assert rendered(overlay("autoscaling.maxReplicas", 1.5), tmp_path)


def test_the_replica_count_is_not_read_while_autoscaling_is_on(tmp_path):
    body = overlay("replicaCount", 1.5, autoscaling=True)
    assert rendered(body, tmp_path, *KEDA)


def test_the_threshold_is_left_alone(tmp_path):
    """KEDA reads the exponent form as a float64, so no rule is added (yaadgaar 482)."""
    assert rendered(overlay("autoscaling.threshold", 1000000, autoscaling=True), tmp_path, *KEDA)


def test_a_max_skew_is_bounded_where_a_float_prints_plainly_and_both_ends_are_exact(tmp_path):
    """Cast through `int64` and still bounded, by choice (yaadgaar 550). The API needs
    maxSkew > 0, so 0 is refused; 999999 is the largest value helm prints without an
    exponent, kept as the ceiling."""
    assert "maxSkew: 1\n" in rendered(overlay("topologySpread.maxSkew", 1), tmp_path)
    stdout = rendered(overlay("topologySpread.maxSkew", 999999), tmp_path)
    assert "maxSkew: 999999\n" in stdout and "e+" not in stdout.split("maxSkew:")[1].split("\n")[0]
    key = "topologySpread.maxSkew"
    for value in (0, "0", 1000000, "1000000", INT32_MAX, NINETEEN_NINES, 1.5, -1, "abc", "", "0123", 1e20, True):
        refused_naming(overlay(key, value), key, tmp_path)


def test_a_max_skew_is_not_read_while_the_constraint_is_off(tmp_path):
    body = overlay("topologySpread.maxSkew", 1.5)
    body["topologySpread"]["enabled"] = False
    assert rendered(body, tmp_path)


def test_a_max_skew_given_by_set_string_is_checked_too():
    ok = set_string("topologySpread.maxSkew=999999")
    assert ok.returncode == 0, ok.stderr
    for value in ("1000000", "0", "0123", NINETEEN_NINES):
        bad = set_string(f"topologySpread.maxSkew={value}")
        assert bad.returncode != 0 and "topologySpread.maxSkew" in bad.stdout + bad.stderr, value


def test_a_zero_timing_knob_still_means_not_rendered(tmp_path):
    assert "terminationGracePeriodSeconds" not in spec(rendered(overlay("terminationGracePeriodSeconds", 0), tmp_path))
    (container,) = spec(rendered(overlay("preStopSleepSeconds", 0), tmp_path))["containers"]
    assert "lifecycle" not in container


DEPLOYMENT = CHART / "templates" / "deployment.yaml"
# (any line that renders the key, the one form that line must take). Both are anchored
# to a whole line with re.M, so a copy in a comment or a trailing comment does not count.
CAST_SITES = {
    "grace": (
        r"^\s*terminationGracePeriodSeconds: \{\{.*$",
        r"^\s*terminationGracePeriodSeconds: \{\{ \. \| int64 \}\}$",
    ),
    "preStop": (
        r"^\s*seconds: \{\{.*$",
        r"^\s*seconds: \{\{ \. \| int64 \}\}$",
    ),
    "maxSkew": (
        r"^\s*- maxSkew: \{\{.*$",
        r"^\s*- maxSkew: \{\{ \.Values\.topologySpread\.maxSkew \| int64 \}\}$",
    ),
}


@pytest.mark.parametrize("site", CAST_SITES)
def test_the_grace_period_pre_stop_sleep_and_skew_render_through_int64(site):
    """Cast, like every other numeric knob (yaadgaar 550). The chart-baseline gate reads
    `| int64` since actions v1.64.0, so the cast no longer costs the gate its view of the
    value. `| int` is not the same cast and does not count. Every value the check in
    render-checks.yaml admits renders the same bytes with or without the cast, so this
    pin is the test that holds the cast in place. Mirrors yadgarhq/iam#172."""
    any_render, cast = CAST_SITES[site]
    text = DEPLOYMENT.read_text()
    renders = re.findall(any_render, text, re.M)
    assert len(renders) == 1, f"{site}: expected one render line in deployment.yaml, found {renders!r}"
    assert re.fullmatch(cast, renders[0]), f"{site}: {renders[0].strip()!r} is not cast through int64"


def test_the_grace_period_and_pre_stop_sleep_are_bounded_where_a_float_prints_plainly(tmp_path):
    """Cast through `int64` and still bounded at 999999, by choice (yaadgaar 550): the
    gate no longer forces the bound, and lifting it is a separate decision. The
    whole-number check runs BEFORE the cast, because `int64` turns 1.5 into 1."""
    assert spec(rendered(overlay("terminationGracePeriodSeconds", 999999), tmp_path))["terminationGracePeriodSeconds"] == 999999
    assert "seconds: 999999\n" in rendered(overlay("preStopSleepSeconds", 999999), tmp_path)
    for key in ("terminationGracePeriodSeconds", "preStopSleepSeconds"):
        for value in (1000000, 1.5, "abc", -1, "0123", 1e20, "", NINETEEN_NINES, "1000000"):
            refused_naming(overlay(key, value), key, tmp_path)


def test_the_grace_period_and_pre_stop_sleep_given_by_set_string_are_checked_too():
    for key in ("terminationGracePeriodSeconds", "preStopSleepSeconds"):
        ok = set_string(f"{key}=999999")
        assert ok.returncode == 0, ok.stderr
        bad = set_string(f"{key}=1000000")
        assert bad.returncode != 0 and key in bad.stdout + bad.stderr


# --- 482 item 2: autoscaling.minReplicas >= 2 while autoscaling is on (D55) --------
# RULED 2026-11-03: D55 makes two pods a correctness property, so KEDA `minReplicaCount`
# below 2 is refused at render. `replicaCount: 0` stays legal while autoscaling is off.


@pytest.mark.parametrize("value", [0, 1, "0", "1"], ids=repr)
def test_autoscaling_on_with_min_replicas_below_two_is_refused_by_name(tmp_path, value):
    body = overlay("autoscaling.minReplicas", value, autoscaling=True)
    combined = refused_naming(body, "autoscaling.minReplicas", tmp_path, *KEDA)
    assert "at least 2" in combined, combined


@pytest.mark.parametrize("value", [2, 3, "2"], ids=repr)
def test_autoscaling_on_with_min_replicas_two_or_more_renders(tmp_path, value):
    body = overlay("autoscaling.minReplicas", value, autoscaling=True)
    assert f"minReplicaCount: {int(value)}\n" in rendered(body, tmp_path, *KEDA)


def test_the_min_replicas_floor_is_not_read_while_autoscaling_is_off(tmp_path):
    body = overlay("autoscaling.minReplicas", 0)
    body["replicaCount"] = 0
    assert "replicas: 0\n" in rendered(body, tmp_path)


# --- 645: autoscaling.maxReplicas >= autoscaling.minReplicas while autoscaling is on ----
# KEDA took `maxReplicaCount` below `minReplicaCount` without a word. Every case keeps
# `minReplicas` at 2 or more, so the D55 floor above cannot be what refuses it, and sets
# `replicaCount` to a third value, so a check that compared against it is caught.


def max_overlay(minimum, maximum, replica_count) -> dict:
    body = overlay("autoscaling.minReplicas", minimum, autoscaling=True)
    body["autoscaling"]["maxReplicas"] = maximum
    body["replicaCount"] = replica_count
    return body


@pytest.mark.parametrize("minimum,maximum", [(2, 0), (2, 1), (5, 4), ("3", "2"), (3, "2")], ids=repr)
def test_autoscaling_on_with_max_replicas_below_min_replicas_is_refused_by_name(tmp_path, minimum, maximum):
    body = max_overlay(minimum, maximum, 1)
    combined = refused_naming(body, "autoscaling.maxReplicas", tmp_path, *KEDA)
    assert "at least `autoscaling.minReplicas`" in combined, combined


@pytest.mark.parametrize("minimum,maximum", [(2, 2), ("3", "3"), (5, 5), (2, 3), ("2", 6)], ids=repr)
def test_autoscaling_on_with_max_replicas_at_or_above_min_replicas_renders(tmp_path, minimum, maximum):
    stdout = rendered(max_overlay(minimum, maximum, 9), tmp_path, *KEDA)
    assert f"minReplicaCount: {int(minimum)}\n" in stdout and f"maxReplicaCount: {int(maximum)}\n" in stdout


def test_the_max_replicas_ceiling_is_not_read_while_autoscaling_is_off(tmp_path):
    body = max_overlay(5, 1, 3)
    body["autoscaling"]["enabled"] = False
    assert "replicas: 3\n" in rendered(body, tmp_path)


def test_a_string_autoscaling_toggle_is_refused_before_the_floor_is_read(tmp_path):
    """`enabled: "false"` reads as off to a human and truthy to Go templates. The bool-shape
    check must refuse it first, so `minReplicas: 0` never slips past the floor behind it."""
    body = overlay("autoscaling.minReplicas", 0)
    body["autoscaling"]["enabled"] = "false"
    combined = refused_naming(body, "autoscaling.enabled", tmp_path, *KEDA)
    assert "must be true or false" in combined and "at least 2" not in combined, combined
