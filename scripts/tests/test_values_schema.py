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
from typing import Any

import pytest
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
# `tls.clientCaSecret`/`clientCaSecretKey` name the CA bundle a verifying
# `tls.clientAuth` reads (B-U5): `templates/deployment.yaml` reads both, and
# `chart/values.yaml` ships neither — unset unless a mode verifies.
# `values.schema.json` still has to declare them, or an adopter naming the
# Secret would be refused by `additionalProperties: false` rather than
# reaching the render check.
EXTRA_LEAVES = (
    "image.digest",
    "networkPolicy.scrapeFrom.namespace",
    "tls.clientCaSecret",
    "tls.clientCaSecretKey",
)

# THE THIRD REQUIRED KEY WITH NO DEFAULT (B-U5, ledger 925): `tls.clientAuth`
# is `required` but UNTYPED — no `enum`, no `type` (ADR-0847, coordinator
# ruling R1). A bare YAML `off` is the boolean false, and the render check's
# named sentence ("write `clientAuth: \"off\"`, quoted") is what must answer
# it; a schema type or enum would pre-empt that sentence. Like the two
# switches it appears nowhere in `chart/values.yaml`, so the oracle injects it.
REQUIRED_UNTYPED_NO_DEFAULT = ("tls.clientAuth",)

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

# THE ONE TYPED, BOUNDED, REQUIRED LEAF THAT DOES SHIP A DEFAULT (ADR-0925 Q-C1,
# ledger 1490): `projectDb.requestTimeoutSeconds` is `type: integer` with a
# `minimum` and `maximum`, and its parent lists it under `required`. Unlike the
# REQUIRED_NO_DEFAULT pair it IS in `chart/values.yaml` (an explicit 30, never
# compiled in), so the oracle derives the leaf from `values.yaml` and then
# overwrites it with the typed node. The bounds are `yadgar_dial`'s
# `min_request_timeout()` / `max_request_timeout()`;
# `tests/chart_request_timeout.rs` holds them to the pinned crate.
REQUIRED_BOUNDED_INTEGER = {
    "projectDb.requestTimeoutSeconds": (14, 59),
    # The four listener limits (ADR-1006). Ranges are
    # `yadgar_lifecycle::serve_tls::ServerLimits`'s; `tests/chart_server_limits.rs`
    # holds them to the pinned crate.
    "server.limits.maxConcurrentStreams": (1, 10000),
    "server.limits.keepaliveIntervalSeconds": (1, 3600),
    "server.limits.keepaliveTimeoutSeconds": (1, 600),
    "server.limits.maxHeaderListBytes": (1024, 1048576),
}


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


def declare_required_untyped(schema: dict, path: str) -> None:
    """`declare_required_no_default`, with an UNTYPED `{}` leaf rather than
    `{"type": "boolean"}`. Mutates `schema`."""
    declare_required_no_default(schema, path)
    cursor = schema
    steps = path.split(".")
    for step in steps[:-1]:
        cursor = cursor["properties"][step]
    cursor["properties"][steps[-1]] = {}


def declare_bounded_integer(schema: dict, path: str, minimum: int, maximum: int) -> None:
    """Overwrite the leaf at `path` with its bounded integer node and list it
    under its parent's `required`. Mutates `schema`."""
    cursor = schema
    steps = path.split(".")
    for step in steps[:-1]:
        cursor = cursor["properties"][step]
    cursor["properties"][steps[-1]] = {"type": "integer", "minimum": minimum, "maximum": maximum}
    cursor["required"] = sorted({*cursor.get("required", []), steps[-1]})


def expected_schema(values: dict) -> dict:
    """The full `properties` / `additionalProperties` shape this chart's schema owes. PURE."""
    schema = expected_node(values, "")
    schema.setdefault("properties", {})
    schema["additionalProperties"] = False
    schema["properties"].setdefault("global", {})
    for path in REQUIRED_NO_DEFAULT:
        declare_required_no_default(schema, path)
    for path in REQUIRED_UNTYPED_NO_DEFAULT:
        declare_required_untyped(schema, path)
    for path in EXTRA_LEAVES:
        declare_extra(schema, path)
    for path, (minimum, maximum) in REQUIRED_BOUNDED_INTEGER.items():
        declare_bounded_integer(schema, path, minimum, maximum)
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


def test_the_request_timeout_is_a_bounded_required_integer():
    schema = load_schema()
    for path, (minimum, maximum) in REQUIRED_BOUNDED_INTEGER.items():
        *parents, leaf_key = path.split(".")
        cursor = schema
        for step in parents:
            cursor = cursor["properties"][step]
        assert cursor["properties"][leaf_key] == {
            "type": "integer",
            "minimum": minimum,
            "maximum": maximum,
        }, path
        assert leaf_key in cursor.get("required", []), path


# ── THE CENSUS: NO type / enum / default / required ANYWHERE UNLESS LISTED ───
#
# ADR-0847 keeps `type`, `enum` and `default` out of this schema; ADR-0845 and
# ADR-0837/ADR-1006 put a few back on purpose. The spot checks above pin SPECIFIC
# leaves. They do not stop a new `type`, `enum`, `default` or `required` landing
# on any OTHER node, nor an `enum` riding in on a leaf that is already typed. The
# census walks EVERY node and compares it with two data tables, so a deliberate
# schema change must edit the table in the same PR (yaadgaar 327).
#
# `TYPED_ON_PURPOSE` maps each leaf path that carries keywords to the exact node
# it must equal; every other leaf must be `{}`. `BLOCKS_ON_PURPOSE` maps each
# block path that carries keywords besides `properties` and
# `additionalProperties` to those keywords; every other block carries none. The
# ROOT is the block path `""`. Prose keys (`description`, `$comment`, `$schema`,
# `title`) are ignored: they decide nothing. `default` is refused everywhere
# with no table to excuse it (ADR-0705: a consequential knob carries no default,
# and a schema `default` is a second source for the chart default).
# The tables are data so ADR-0705's REQUIRED list (yaadgaar 336) can extend them.

TYPED_ON_PURPOSE: dict[str, dict[str, Any]] = {
    "projectDb.requestTimeoutSeconds": {"type": "integer", "minimum": 14, "maximum": 59},
    "projectDb.tls.enabled": {"type": "boolean"},
    "tls.enabled": {"type": "boolean"},
    "server.limits.maxConcurrentStreams": {"type": "integer", "minimum": 1, "maximum": 10000},
    "server.limits.keepaliveIntervalSeconds": {"type": "integer", "minimum": 1, "maximum": 3600},
    "server.limits.keepaliveTimeoutSeconds": {"type": "integer", "minimum": 1, "maximum": 600},
    "server.limits.maxHeaderListBytes": {"type": "integer", "minimum": 1024, "maximum": 1048576},
}

BLOCKS_ON_PURPOSE: dict[str, dict[str, Any]] = {
    "": {"required": ["server"]},
    "projectDb": {"required": ["requestTimeoutSeconds"]},
    "projectDb.tls": {"required": ["enabled"]},
    "tls": {
        "required": [
            "clientAuth",
            "enabled",
        ],
    },
    "server.limits": {
        "required": [
            "keepaliveIntervalSeconds",
            "keepaliveTimeoutSeconds",
            "maxConcurrentStreams",
            "maxHeaderListBytes",
        ],
    },
}

BOUNDED_LEAF = "projectDb.requestTimeoutSeconds"
CENSUS_PROSE = frozenset({"description", "$comment", "$schema", "title"})
CENSUS_STRUCTURE = frozenset({"properties", "additionalProperties"})


def _census_keywords(node: dict[str, Any], skip: frozenset[str]) -> dict[str, Any]:
    kept = {key: value for key, value in node.items() if key not in skip}
    if "required" in kept:
        kept["required"] = sorted(kept["required"])
    return kept


def census_failures(
    schema: dict[str, Any],
    typed: dict[str, dict[str, Any]] | None = None,
    blocks: dict[str, dict[str, Any]] | None = None,
) -> list[str]:
    """Every node whose keywords differ from the tables above. PURE."""
    typed = TYPED_ON_PURPOSE if typed is None else typed
    blocks = BLOCKS_ON_PURPOSE if blocks is None else blocks
    failures: list[str] = []
    seen_leaves: set[str] = set()
    seen_blocks: set[str] = set()

    def walk(node: dict[str, Any], path: str) -> None:
        where = path or "<root>"
        if "default" in node:
            failures.append(f"{where}: carries a `default`")
        if "enum" in node:
            failures.append(f"{where}: carries an `enum`")
        if "properties" in node:
            seen_blocks.add(path)
            expected = _census_keywords(blocks.get(path, {}), frozenset())
            actual = _census_keywords(node, CENSUS_PROSE | CENSUS_STRUCTURE)
            if actual != expected:
                failures.append(f"{where}: block keywords {actual} != {expected}")
            for key, child in node["properties"].items():
                walk(child, f"{path}.{key}" if path else key)
        else:
            seen_leaves.add(path)
            expected = _census_keywords(typed.get(path, {}), frozenset())
            actual = _census_keywords(node, CENSUS_PROSE)
            if actual != expected:
                failures.append(f"{where}: leaf {actual} != {expected}")

    walk(schema, "")
    for path in sorted(set(typed) - seen_leaves):
        failures.append(f"{path}: TYPED_ON_PURPOSE names a leaf the schema does not have")
    for path in sorted(set(blocks) - seen_blocks):
        failures.append(f"{path or '<root>'}: BLOCKS_ON_PURPOSE names a block the schema does not have")
    return failures


def a_census_untyped_leaf(schema: dict[str, Any]) -> str:
    """The first leaf path (sorted) that no table excuses. PURE."""
    leaves: list[str] = []

    def walk(node: dict[str, Any], path: str) -> None:
        if "properties" in node:
            for key, child in node["properties"].items():
                walk(child, f"{path}.{key}" if path else key)
        elif path not in TYPED_ON_PURPOSE and node == {}:
            leaves.append(path)

    walk(schema, "")
    return sorted(leaves)[0]


def census_node(schema: dict[str, Any], path: str) -> dict[str, Any]:
    node = schema
    for step in path.split(".") if path else []:
        node = node["properties"][step]
    return node


def test_the_census_is_clean_on_the_shipped_schema() -> None:
    assert census_failures(load_schema()) == []


def test_the_census_tables_name_only_nodes_that_carry_keywords() -> None:
    assert all(TYPED_ON_PURPOSE.values()), "an empty TYPED_ON_PURPOSE entry excuses nothing"
    assert all(BLOCKS_ON_PURPOSE.values()), "an empty BLOCKS_ON_PURPOSE entry excuses nothing"


def test_the_census_ignores_prose() -> None:
    mutated = copy.deepcopy(load_schema())
    mutated["$comment"] = "reworded"
    census_node(mutated, "tls.enabled")["description"] = "reworded"
    census_node(mutated, a_census_untyped_leaf(mutated))["description"] = "reworded"
    assert census_failures(mutated) == []


def census_mutations() -> dict[str, Any]:
    """Name -> mutator(schema) for every row of the red table. Each must redden."""

    def on_untyped(change: dict[str, Any]):
        def mutate(schema: dict[str, Any]) -> None:
            census_node(schema, a_census_untyped_leaf(schema)).update(change)

        return mutate

    def on_node(path: str, change: dict[str, Any]):
        def mutate(schema: dict[str, Any]) -> None:
            census_node(schema, path).update(change)

        return mutate

    def delete_key(path: str, key: str):
        def mutate(schema: dict[str, Any]) -> None:
            del census_node(schema, path)[key]

        return mutate

    def replace_node(path: str, node: dict[str, Any]):
        def mutate(schema: dict[str, Any]) -> None:
            parent, _, leaf = path.rpartition(".")
            census_node(schema, parent)["properties"][leaf] = node

        return mutate

    def drop_required(path: str, name: str):
        def mutate(schema: dict[str, Any]) -> None:
            census_node(schema, path)["required"].remove(name)

        return mutate

    return {
        # keyword added where the table lists none
        "type on an untyped leaf": on_untyped({"type": "string"}),
        "enum on an untyped leaf": on_untyped({"enum": ["a", "b"]}),
        "default on an untyped leaf": on_untyped({"default": "x"}),
        "required on an untyped leaf": on_untyped({"required": ["x"]}),
        "minimum on an untyped leaf": on_untyped({"minimum": 1}),
        "enum on tls.clientAuth": on_node("tls.clientAuth", {"enum": ["off", "optional", "required"]}),
        "type on tls.clientAuth": on_node("tls.clientAuth", {"type": "string"}),
        "default on tls.clientAuth": on_node("tls.clientAuth", {"default": "off"}),
        # keyword riding on a leaf that is already typed on purpose
        "default on tls.enabled": on_node("tls.enabled", {"default": True}),
        "enum on tls.enabled": on_node("tls.enabled", {"enum": [True]}),
        "default on a bounded integer": on_node(BOUNDED_LEAF, {"default": 5}),
        "enum on a bounded integer": on_node(BOUNDED_LEAF, {"enum": [5]}),
        # a typed leaf drifts
        "bound raised": on_node(BOUNDED_LEAF, {"maximum": TYPED_ON_PURPOSE[BOUNDED_LEAF]["maximum"] + 1}),
        "bound lowered": on_node(BOUNDED_LEAF, {"minimum": TYPED_ON_PURPOSE[BOUNDED_LEAF]["minimum"] + 1}),
        "bound dropped": delete_key(BOUNDED_LEAF, "maximum"),
        "type changed": on_node(BOUNDED_LEAF, {"type": "number"}),
        # a typed leaf loses its keywords (the reverse direction)
        "tls.enabled loses its type": replace_node("tls.enabled", {}),
        "bounded integer loses its type": replace_node(BOUNDED_LEAF, {}),
        # blocks
        "default on a block": on_node("tls", {"default": {}}),
        "type on a block that has none": on_node("tls", {"type": "object"}),
        "enum on a block": on_node("tls", {"enum": [{}]}),
        "extra required on a block": on_node("tls", {"required": ["enabled", "clientAuth", "clientCaSecret"]}),
        "required key dropped from tls": drop_required("tls", "clientAuth"),
        "required dropped from a block": delete_key("tls", "required"),
        "required key dropped from projectDb": drop_required("projectDb", "requestTimeoutSeconds"),
        "required key dropped from projectDb.tls": drop_required("projectDb.tls", "enabled"),
        "required key dropped from server.limits": drop_required("server.limits", "maxHeaderListBytes"),
        "required deleted from server.limits": delete_key("server.limits", "required"),
        "projectDb.tls.enabled loses its type": replace_node("projectDb.tls.enabled", {}),
        "server.limits leaf loses its type": replace_node("server.limits.maxConcurrentStreams", {}),
        # root
        "type on the root": on_node("", {"type": "object"}),
        "default on the root": on_node("", {"default": {}}),
        "extra required on the root": on_node("", {"required": ["server", "tls"]}),
        "required key dropped from the root": drop_required("", "server"),
        "required deleted from the root": delete_key("", "required"),
    }


@pytest.mark.parametrize("name", sorted(census_mutations()))
def test_the_census_reddens_on(name: str) -> None:
    mutated = copy.deepcopy(load_schema())
    census_mutations()[name](mutated)
    assert census_failures(mutated) != [], name


def test_a_stale_typed_entry_reddens_the_census() -> None:
    typed = {**TYPED_ON_PURPOSE, "no.such.leaf": {"type": "string"}}
    assert census_failures(load_schema(), typed=typed) != []


def test_a_stale_block_entry_reddens_the_census() -> None:
    blocks = {**BLOCKS_ON_PURPOSE, "no.such.block": {"required": ["x"]}}
    assert census_failures(load_schema(), blocks=blocks) != []


def test_a_typed_table_missing_an_entry_reddens_the_census() -> None:
    typed = {path: node for path, node in TYPED_ON_PURPOSE.items() if path != "tls.enabled"}
    assert census_failures(load_schema(), typed=typed) != []


def test_a_block_table_missing_an_entry_reddens_the_census() -> None:
    blocks = {path: node for path, node in BLOCKS_ON_PURPOSE.items() if path != "tls"}
    assert census_failures(load_schema(), blocks=blocks) != []
