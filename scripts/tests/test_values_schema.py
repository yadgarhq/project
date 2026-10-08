"""THE CLOSED `values.schema.json` (ledger 990, ADR-0847, ADR-0850).

ADR-0847 rules that every chart's `values.schema.json` closes its own key set —
`additionalProperties: false` at every object level — so a typo in an adopter's
values file refuses at render instead of being silently accepted. ADR-0850
narrows that to CLOSURE PER CHART: this chart closes the keys it owns, declares
`global` open (the parent closes it), and leaves two maps open because an
adopter extends them with keys this chart never names (`resources`,
`rollingUpdate`). Types, toggles and cross-key rules stay in
`templates/render-checks.yaml` (ADR-0797) — this file asserts SHAPE, never a
value.

THE ORACLE IS A RE-DERIVATION, NOT A TRANSCRIPTION. `expected_schema` rebuilds
the schema's shape straight from `chart/values.yaml` plus the two small,
explicit tuples below (`OPEN`, the maps a block is deliberately left open;
`EXTRA_LEAVES`, the keys a template reads that `values.yaml` never declares) —
the same rule `gen_schema.py` (yadgarhq/docs, scratchpad) used to draft this
file. A schema that drifts from `values.yaml` — a key renamed, a block closed
that should stay open, a leaf dropped — reddens
`test_the_schema_matches_its_own_values_yaml` on that mismatch alone, which is
also why the four mutation tests below (delete root `additionalProperties`,
delete `global`, delete one extra, close one open map) all exercise the SAME
assertion rather than four different ones: a mismatch in any direction fails
it.

THE RED-CASE TABLE (module docstring's half of ADR-0645's render-neutrality
sweep) asserts the SCHEMA KEY NAME AND ITS JSON PATH FRAGMENT ONLY, never
helm's wording — helm 3.18.4 prints
`- <path>: Additional property X is not allowed` where 3.20.2 and 4.3.0 print
`- at '/<path>': additional properties 'X' not allowed`, and this suite is
read on all three. The existing render-check sentences
(`test_render_checks.py`, `test_autoscaling.py`) are untouched by this file and
are the render-neutrality evidence for the toggle- and shape-typed rows the
brief's table also lists (`autoscaling.enabled: "false"`, `autoscaling: "x"`,
a deleted `autoscaling`) — this schema declares those keys untyped, on
purpose, so it never pre-empts them with a schema-shaped refusal of its own.

Run: python3 -m pytest scripts/tests/ -q
"""

from __future__ import annotations

import copy
import json
import re
import shutil
from pathlib import Path

import yaml

from test_render_checks import CHART, CHART_NAME, REPO, helm, objects, render

SCHEMA_PATH = CHART / "values.schema.json"
VALUES_PATH = CHART / "values.yaml"

# ── THE ORACLE'S INPUTS, WRITTEN DOWN (brief §3.4, §4) ───────────────────────

# Paths `values.yaml` declares as non-empty maps that this chart deliberately
# leaves OPEN (schema `{}`) rather than closing — an adopter sets keys under
# these two that this chart never names. `global` is not here: it never
# appears in THIS chart's `values.yaml` at all, and is declared open at the
# root unconditionally, because the PARENT forwards it (ADR-0850).
OPEN = ("resources", "rollingUpdate")

# Paths a template reads (`grep -rhoE '\.Values(\.[A-Za-z0-9_-]+)+' templates`)
# that `values.yaml` does not declare. Declaring them as leaves turns their
# parent block from an open map into a closed one with exactly this key.
#
# `tls.clientAuth`/`clientCaSecret`/`clientCaSecretKey` are B-U5E's folded
# expand (ledger 965): `templates/deployment.yaml` reads all three, and
# `chart/values.yaml` ships none of them — left unset on purpose, so a
# values file that does not set them renders byte-identical to
# origin/main's (K-8). `values.schema.json` still has to declare them, or
# an adopter setting `tls.clientAuth` would be refused by `additionalProperties:
# false` rather than reaching the render check.
EXTRA_LEAVES = (
    "image.digest",
    "networkPolicy.scrapeFrom.namespace",
    "tls.clientAuth",
    "tls.clientCaSecret",
    "tls.clientCaSecretKey",
)

# THE TWO NAMED EXCEPTIONS (ledger 965, ADR-0845): `tls.enabled` and
# `projectDb.tls.enabled` are the only typed, required leaves this chart's
# schema carries. Every other leaf stays untyped `{}`.
#
# SCHEMA-ONLY, INDEPENDENT OF `values.yaml`, and that independence is the
# point rather than an implementation detail: this chart ships NO default
# for either key (ADR-0845 — a chart default would be exactly the
# compiled-in default the ADR forbids, one layer up), so neither key
# appears in `chart/values.yaml` at all any more. The oracle cannot derive
# either leaf by walking `values.yaml`'s own content, the way every other
# leaf and every `EXTRA_LEAVES` entry is derived; `declare_required_no_default`
# below injects both directly, the same way `declare_extra` injects a path
# a template reads that `values.yaml` never declares.
REQUIRED_NO_DEFAULT = ("tls.enabled", "projectDb.tls.enabled")


def load_values() -> dict:
    return yaml.safe_load(VALUES_PATH.read_text()) or {}


def load_schema() -> dict:
    return json.loads(SCHEMA_PATH.read_text())


def expected_node(value, path: str):
    """The schema node `gen_schema.py` would draft at `path`, from `value` alone. PURE."""
    if path in OPEN:
        return {}
    if isinstance(value, dict):
        if not value:
            return {}
        return {
            "properties": {
                key: expected_node(sub, f"{path}.{key}" if path else key)
                for key, sub in value.items()
            },
            "additionalProperties": False,
        }
    return {}


def declare_extra(schema: dict, path: str) -> None:
    """Declare `path` as a leaf, closing every block along the way. Mutates `schema`."""
    cursor = schema
    steps = path.split(".")
    for step in steps[:-1]:
        cursor.setdefault("properties", {})
        cursor.setdefault("additionalProperties", False)
        cursor = cursor["properties"].setdefault(step, {})
    cursor.setdefault("properties", {})
    cursor.setdefault("additionalProperties", False)
    cursor["properties"].setdefault(steps[-1], {})


def declare_required_no_default(schema: dict, path: str) -> None:
    """Declare `path` as a TYPED, REQUIRED leaf (ADR-0845), closing every
    block along the way exactly as `declare_extra` does. Mutates `schema`.

    UNLIKE `declare_extra`, this OVERWRITES the leaf with `{"type":
    "boolean"}` rather than `setdefault`-ing `{}` — the leaf is typed, not
    untyped — and adds the final step to its PARENT's `required` list
    (merged with whatever is already there, so this chart's own two calls
    for `tls.enabled` and `projectDb.tls.enabled` cannot clobber each
    other's `required` list: they are different parents).
    """
    cursor = schema
    steps = path.split(".")
    for step in steps[:-1]:
        cursor.setdefault("properties", {})
        cursor.setdefault("additionalProperties", False)
        cursor = cursor["properties"].setdefault(step, {})
    cursor.setdefault("properties", {})
    cursor.setdefault("additionalProperties", False)
    cursor["properties"][steps[-1]] = {"type": "boolean"}
    cursor["required"] = sorted({*cursor.get("required", []), steps[-1]})


def expected_schema(values: dict) -> dict:
    """The full `properties` / `additionalProperties` shape this chart's schema owes. PURE."""
    schema = expected_node(values, "")
    schema.setdefault("properties", {})
    schema["additionalProperties"] = False
    schema["properties"].setdefault("global", {})
    for path in REQUIRED_NO_DEFAULT:
        declare_required_no_default(schema, path)
    for path in EXTRA_LEAVES:
        declare_extra(schema, path)
    return schema


def structural_shape(schema: dict) -> dict:
    """The part of a loaded schema the oracle speaks about — no `$schema`/`title`/`$comment`."""
    return {key: val for key, val in schema.items() if key in ("properties", "additionalProperties")}


def structural_failures(schema: dict, values: dict) -> list[str]:
    """How `schema`'s shape disagrees with `expected_schema(values)`. PURE."""
    found = structural_shape(schema)
    wanted = expected_schema(values)
    if found != wanted:
        return [f"schema shape does not match values.yaml + OPEN + EXTRA_LEAVES:\nfound:    {found}\nexpected: {wanted}"]
    return []


# ── STRUCTURE (PURE — no helm) ────────────────────────────────────────────────


def test_the_schema_is_valid_json_with_the_right_header():
    schema = load_schema()
    assert schema.get("$schema") == "http://json-schema.org/draft-07/schema#"
    assert schema.get("title") == f"yadgar/{CHART_NAME}"
    comment = schema.get("$comment", "")
    assert comment, "the schema carries no $comment explaining its closure (brief §3.7)"
    for must_mention in ("ADR-0850", "resources", "rollingUpdate", "image.digest", "scrapeFrom"):
        assert must_mention in comment, f"$comment does not mention {must_mention!r}"


def test_the_schema_matches_its_own_values_yaml():
    failures = structural_failures(load_schema(), load_values())
    assert failures == [], "\n".join(failures)


def schema_node(schema: dict, path: str):
    """Walk `schema["properties"][s1]["properties"][s2]...` for every step in `path`. PURE."""
    node = schema
    for step in path.split("."):
        node = node["properties"][step]
    return node


def test_open_maps_are_declared_open():
    schema = load_schema()
    for path in OPEN:
        assert schema_node(schema, path) == {}, f"{path} is not declared as an open map"


def test_extras_are_declared_under_a_closed_block():
    schema = load_schema()
    for path in EXTRA_LEAVES:
        steps = path.split(".")
        parent_path = ".".join(steps[:-1])
        parent = schema_node(schema, parent_path) if parent_path else schema
        assert parent.get("additionalProperties") is False, f"{parent_path or '(root)'} (parent of extra {path}) is not closed"
        assert schema_node(schema, path) == {}, f"{path} is not declared as a leaf"


def test_global_is_declared_open_at_the_root():
    schema = load_schema()
    assert schema["properties"].get("global") == {}, "`global` must be an open map at the root"


# ── MUTATIONS (brief §5 structural row — each must redden the match above) ──


def test_deleting_root_additionalProperties_reddens():
    schema = load_schema()
    del schema["additionalProperties"]
    assert structural_failures(schema, load_values()), (
        "deleting the root `additionalProperties: false` did not redden the match — "
        "an adopter typo at the root would pass silently"
    )


def test_deleting_global_reddens():
    schema = load_schema()
    del schema["properties"]["global"]
    assert structural_failures(schema, load_values()), (
        "deleting `global` did not redden the match"
    )


def test_deleting_an_extra_reddens():
    schema = load_schema()
    del schema["properties"]["image"]["properties"]["digest"]
    assert structural_failures(schema, load_values()), (
        "deleting the `image.digest` extra did not redden the match — ci-release's "
        "own write would then refuse at render"
    )


def test_closing_an_open_map_reddens():
    schema = load_schema()
    schema["properties"]["resources"] = {"properties": {}, "additionalProperties": False}
    assert structural_failures(schema, load_values()), (
        "closing the open `resources` map did not redden the match — an adopter's "
        "own resource key (e.g. ephemeral-storage) would then be refused"
    )


# ── THE RED-CASE TABLE (brief §5), KEY + PATH ONLY, NEVER HELM'S WORDING ────

ROOT_PATTERN = re.compile(r"(^''|^\(root\)|^$)")


def refusal_names(stderr: str, key: str) -> bool:
    return key in stderr


def test_red_root_typo_names_the_key():
    overlay = CHART.parent / "scratch-root-typo.yaml"
    overlay.write_text("autoscalng:\n  enabled: true\n")
    try:
        result = render(CHART, "-f", str(overlay))
    finally:
        overlay.unlink()
    assert result.returncode != 0, result.stdout
    assert refusal_names(result.stderr, "autoscalng"), result.stderr


def test_red_nested_typo_names_the_key_and_parent_path():
    overlay = CHART.parent / "scratch-nested-typo.yaml"
    overlay.write_text("autoscaling:\n  enabeld: true\n")
    try:
        result = render(CHART, "-f", str(overlay))
    finally:
        overlay.unlink()
    assert result.returncode != 0, result.stdout
    assert refusal_names(result.stderr, "enabeld"), result.stderr
    assert "autoscaling" in result.stderr, result.stderr


def test_red_two_down_typo_names_the_key_and_path():
    overlay = CHART.parent / "scratch-two-down-typo.yaml"
    overlay.write_text("networkPolicy:\n  scrapeFrom:\n    namespac: x\n")
    try:
        result = render(CHART, "-f", str(overlay))
    finally:
        overlay.unlink()
    assert result.returncode != 0, result.stdout
    assert refusal_names(result.stderr, "namespac"), result.stderr
    assert "scrapeFrom" in result.stderr, result.stderr


def test_green_open_map_resources_accepts_an_unknown_key():
    overlay = CHART.parent / "scratch-open-resources.yaml"
    overlay.write_text("resources:\n  foo:\n    bar: 1\n")
    try:
        result = render(CHART, "-f", str(overlay))
    finally:
        overlay.unlink()
    assert result.returncode == 0, result.stderr
    assert len(objects(result.stdout)) == len(objects(render(CHART).stdout))


def test_green_open_map_rollingUpdate_accepts_an_unknown_key():
    overlay = CHART.parent / "scratch-open-rollingupdate.yaml"
    overlay.write_text("rollingUpdate:\n  partition: 1\n")
    try:
        result = render(CHART, "-f", str(overlay))
    finally:
        overlay.unlink()
    assert result.returncode == 0, result.stderr


def test_green_open_map_global_accepts_an_unknown_key():
    overlay = CHART.parent / "scratch-open-global.yaml"
    overlay.write_text("global:\n  whatever: 1\n")
    try:
        result = render(CHART, "-f", str(overlay))
    finally:
        overlay.unlink()
    assert result.returncode == 0, result.stderr


def test_green_extra_image_digest_is_accepted():
    overlay = CHART.parent / "scratch-extra-digest.yaml"
    overlay.write_text("image:\n  digest: sha256:" + "a" * 64 + "\n")
    try:
        result = render(CHART, "-f", str(overlay))
    finally:
        overlay.unlink()
    assert result.returncode == 0, result.stderr


def test_green_untyped_leaf_accepts_a_set_string():
    result = render(CHART, "--set-string", "replicaCount=2")
    assert result.returncode == 0, result.stderr


def test_red_lint_names_the_root_typo():
    overlay = CHART.parent / "scratch-lint-typo.yaml"
    overlay.write_text("autoscalng:\n  enabled: true\n")
    try:
        result = helm("lint", "--strict", str(CHART), "-f", str(overlay))
    finally:
        overlay.unlink()
    assert result.returncode != 0, result.stdout
    assert "autoscalng" in (result.stdout + result.stderr)


def test_the_default_render_object_count_is_unchanged():
    """THE RENDER-NEUTRALITY ROW (ADR-0645): the schema refuses nothing the chart's
    own defaults, or any value this suite's siblings already exercise, write."""
    result = render(CHART)
    assert result.returncode == 0, result.stderr
    assert len(objects(result.stdout)) == 4, objects(result.stdout)
