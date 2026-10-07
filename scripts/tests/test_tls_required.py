"""`tls.enabled` / `projectDb.tls.enabled` HAVE NO DEFAULT (ADR-0845, ledger 965 K-1).

The binary refuses the boot on anything but exactly "1" or "0" for
`LISTEN_TLS_ENABLED` / `PROJECT_DB_TLS_ENABLED` (`src/serve.rs`,
`src/upstream.rs`). This chart renders both UNCONDITIONALLY, as
`ternary "1" "0" <raw value>`, so the shape has to be refused before that
line runs rather than left to render whatever `ternary` makes of a bad
value.

NEITHER KEY HAS A DEFAULT ANYWHERE IN THIS CHART EITHER. `chart/values.yaml`
ships no `enabled` under `tls` or `projectDb.tls` any more: a chart default
would be exactly the compiled-in default ADR-0845 forbids, one layer up.
`chart/ci/values.yaml` states both `true` — read by the shared offline gates
(`helm-lint`, `d80_portability.py`, `service_immutable.py`) and by this
suite's own `render()` helper (`test_render_checks.py::ci_values_flags`) —
so every ordinary case in this file either overrides one of the two keys
explicitly or relies on that `true`, never on a chart default that no
longer exists.

TWO LAYERS CATCH DIFFERENT SHAPES, MEASURED RATHER THAN ASSUMED (K-1).
`values.schema.json`'s `required` + `type: boolean` on the `enabled` leaf
catches: the key missing entirely, a null value (helm deletes a
null-valued key a chart declares, and also leaves a key that was never
declared anywhere simply absent — both collapse to the same "missing
property" shape), and any non-boolean scalar — ALL OF THESE REFUSE BEFORE
THE TEMPLATE RUNS AT ALL, with helm's OWN schema-validation wording, which
differs across helm versions (3.18.4: `tls: enabled is required` /
`tls.enabled: Invalid type. Expected: boolean, given: string`; 4.3.0:
`at '/tls': missing property 'enabled'` / `at '/tls/enabled': got string,
want boolean`). The one constant across every version is the wrapper
phrase `values don't meet the specifications of the schema(s)` plus the
key and parent names, so that is all this file asserts for a
schema-caught shape (CI pins 3.18.4; this suite also runs on 3.20.2 and
4.3.0).

`templates/render-checks.yaml`'s own guard catches what the schema
cannot: `tls` (or `projectDb.tls`) itself being null or a non-map, which
the schema's nested `properties`/`required` never inspects because
nothing in this chart's schema types the BLOCK, only the `enabled` leaf
inside it. `tls: {enabled: null}` and an absent `tls.enabled` both still
reach the SCHEMA, never the template guard — this is measured, not
assumed: helm deletes a null-valued key the chart declares AND leaves a
never-declared key simply missing, and the schema's `required` refuses
both shapes identically before any template runs. Only `tls: null` and
`tls: "x"` (the whole BLOCK, not the leaf) reach the template guard,
because the schema's `properties`/`required` keywords are no-ops against
an instance that is not an object at all. That guard's refusal is OURS, so
it is asserted exactly.

Run: python3 -m pytest scripts/tests/ -q
"""

from __future__ import annotations

import shutil
from pathlib import Path

from test_render_checks import CHART, render

SCHEMA_WRAPPER = "values don't meet the specifications of the schema(s)"

# ── `tls` (the LISTENER) ──────────────────────────────────────────────────

# THE TEMPLATE'S OWN `kindIs "bool"` ARM FOR `tls.enabled` IS SHADOWED, on
# every path `helm template`/`helm install` take, by the schema's own
# `type: boolean` on the same leaf — the schema refuses first (measured
# above, in the module docstring). It stays in `templates/render-checks.yaml`
# for `--disable-openapi-validation` and is not asserted here: there is no
# shape reachable through the normal path that exercises it rather than the
# schema. `test_tls_block_null_refuses_at_the_template_guard` below is the
# one shape this chart's normal path reaches at the template layer instead.
TLS_MAP_GUARD_SENTENCE = "`tls` must be a map and is invalid (null)"


def overlay(body: str, destination: Path) -> Path:
    destination.mkdir(parents=True, exist_ok=True)
    path = destination / "values.yaml"
    path.write_text(body)
    return path


def chart_without_ci_override(destination: Path) -> Path:
    """A copy of `CHART` with `ci/values.yaml` removed, so `render()`'s own
    auto-injected override (which states both switches `true`) cannot paper
    over the truly-absent shape a case exists to exercise."""
    copy = destination / "chart"
    shutil.copytree(CHART, copy)
    (copy / "ci" / "values.yaml").unlink()
    return copy


def test_tls_enabled_null_refuses_at_the_schema(tmp_path):
    """A null `enabled` DELETES the key from the merge (helm's own rule for
    a null-valued key any source declares), which is indistinguishable from
    never having declared it at all — both are `required`'s business."""
    values = overlay("tls:\n  enabled: null\n", tmp_path)
    result = render(CHART, "-f", str(values))
    assert result.returncode != 0, result.stdout
    assert SCHEMA_WRAPPER in result.stderr, result.stderr
    assert "tls" in result.stderr and "enabled" in result.stderr, result.stderr


def test_tls_enabled_truly_absent_refuses_at_the_schema(tmp_path):
    """No override AT ALL, and no chart default either (ADR-0845) — not a
    null that merely deletes a default this chart does not ship. A chart
    copy with `ci/values.yaml` removed is what makes `render()`'s own
    auto-injected override not stand in for the absence this case proves."""
    result = render(chart_without_ci_override(tmp_path))
    assert result.returncode != 0, result.stdout
    assert SCHEMA_WRAPPER in result.stderr, result.stderr
    assert "tls" in result.stderr and "enabled" in result.stderr, result.stderr


def test_tls_enabled_wrong_type_refuses_at_the_schema(tmp_path):
    values = overlay('tls:\n  enabled: "true"\n', tmp_path)
    result = render(CHART, "-f", str(values))
    assert result.returncode != 0, result.stdout
    assert SCHEMA_WRAPPER in result.stderr, result.stderr
    assert "tls" in result.stderr and "enabled" in result.stderr, result.stderr


def test_tls_block_null_refuses_at_the_template_guard(tmp_path):
    """The schema's nested `properties`/`required` never runs against a null
    block — JSON Schema only inspects `properties` when the instance IS an
    object — so this shape reaches `templates/render-checks.yaml` instead,
    which is why its sentence is asserted exactly rather than the schema's."""
    values = overlay("tls: null\n", tmp_path)
    result = render(CHART, "-f", str(values))
    assert result.returncode != 0, result.stdout
    assert SCHEMA_WRAPPER not in result.stderr, (
        "a null `tls` block was refused by the schema, so the template guard "
        f"this case exists to exercise never ran: {result.stderr}"
    )
    assert TLS_MAP_GUARD_SENTENCE in result.stderr, result.stderr


def test_tls_block_not_a_map_refuses_at_the_template_guard(tmp_path):
    values = overlay('tls: "x"\n', tmp_path)
    result = render(CHART, "-f", str(values))
    assert result.returncode != 0, result.stdout
    assert "`tls` must be a map and is string" in result.stderr, result.stderr


def test_tls_enabled_true_renders_unconditionally(tmp_path):
    result = render(CHART, "--set", "tls.enabled=true", "--set", "tls.certSecret=x")
    assert result.returncode == 0, result.stderr
    assert 'name: LISTEN_TLS_ENABLED\n              value: "1"' in result.stdout


def test_tls_enabled_false_renders_the_variable_too(tmp_path):
    """UNCONDITIONAL means an explicit `false` renders the variable too,
    which an `if` never did. `chart/ci/values.yaml` states `true`, so
    `false` is stated here explicitly rather than relied on as a default —
    there is no default to rely on (ADR-0845)."""
    result = render(CHART, "--set", "tls.enabled=false")
    assert result.returncode == 0, result.stderr
    assert 'name: LISTEN_TLS_ENABLED\n              value: "0"' in result.stdout


# ── `projectDb.tls` (the DIAL) — the identical shapes ──────────────────────


def test_projectdb_tls_enabled_null_refuses_at_the_schema(tmp_path):
    values = overlay("projectDb:\n  tls:\n    enabled: null\n", tmp_path)
    result = render(CHART, "-f", str(values))
    assert result.returncode != 0, result.stdout
    assert SCHEMA_WRAPPER in result.stderr, result.stderr
    assert "projectDb" in result.stderr and "enabled" in result.stderr, result.stderr


def test_projectdb_tls_enabled_truly_absent_refuses_at_the_schema(tmp_path):
    result = render(chart_without_ci_override(tmp_path))
    assert result.returncode != 0, result.stdout
    assert SCHEMA_WRAPPER in result.stderr, result.stderr
    assert "projectDb" in result.stderr and "enabled" in result.stderr, result.stderr


def test_projectdb_tls_enabled_wrong_type_refuses_at_the_schema(tmp_path):
    values = overlay('projectDb:\n  tls:\n    enabled: "true"\n', tmp_path)
    result = render(CHART, "-f", str(values))
    assert result.returncode != 0, result.stdout
    assert SCHEMA_WRAPPER in result.stderr, result.stderr
    # THE PATH SEGMENT, NOT ONLY THE BARE KEY NAME: `enabled` alone could be
    # any leaf this chart's schema types, and this case exists to pin the
    # ONE under `projectDb.tls`, not merely that some `enabled` was wrong.
    assert "projectDb" in result.stderr, result.stderr
    assert "tls" in result.stderr, result.stderr
    assert "enabled" in result.stderr, result.stderr


def test_projectdb_tls_block_null_refuses_at_the_template_guard(tmp_path):
    values = overlay("projectDb:\n  tls: null\n", tmp_path)
    result = render(CHART, "-f", str(values))
    assert result.returncode != 0, result.stdout
    assert SCHEMA_WRAPPER not in result.stderr, result.stderr
    assert "`projectDb.tls` must be a map and is invalid (null)" in result.stderr, result.stderr


def test_projectdb_tls_enabled_true_renders_unconditionally(tmp_path):
    result = render(
        CHART,
        "--set",
        "projectDb.tls.enabled=true",
        "--set",
        "projectDb.tls.caSecret=x",
    )
    assert result.returncode == 0, result.stderr
    assert 'name: PROJECT_DB_TLS_ENABLED\n              value: "1"' in result.stdout


def test_projectdb_tls_enabled_false_renders_the_variable_too(tmp_path):
    result = render(CHART, "--set", "projectDb.tls.enabled=false")
    assert result.returncode == 0, result.stderr
    assert 'name: PROJECT_DB_TLS_ENABLED\n              value: "0"' in result.stdout


# ── `tls.clientAuth` (B-U5E, folded expand) — nested under `tls.enabled` ──


def test_client_auth_absent_is_render_neutral(tmp_path):
    """Unset renders nothing for it, whether `tls.enabled` is true or false —
    the whole point of an expand (K-8)."""
    result = render(CHART, "--set", "tls.enabled=true", "--set", "tls.certSecret=x")
    assert result.returncode == 0, result.stderr
    assert "LISTEN_TLS_CLIENT_AUTH" not in result.stdout
    assert "LISTEN_TLS_CLIENT_CA_FILE" not in result.stdout
    assert "name: client-ca" not in result.stdout


def test_client_auth_present_but_tls_disabled_renders_nothing(tmp_path):
    """Mutual TLS on the listener is meaningless without the encrypted
    transport it runs inside (convention ruling 3): `tls.enabled: false`
    must render no client-auth env at all, however `clientAuth` is set."""
    result = render(
        CHART,
        "--set",
        "tls.enabled=false",
        "--set-string",
        "tls.clientAuth=off",
    )
    assert result.returncode == 0, result.stderr
    assert "LISTEN_TLS_CLIENT_AUTH" not in result.stdout


def test_client_auth_off_is_the_only_accepted_value(tmp_path):
    result = render(
        CHART,
        "--set",
        "tls.enabled=true",
        "--set",
        "tls.certSecret=x",
        "--set-string",
        "tls.clientAuth=off",
    )
    assert result.returncode == 0, result.stderr
    assert 'name: LISTEN_TLS_CLIENT_AUTH\n              value: "off"' in result.stdout


def test_client_auth_unquoted_off_is_refused_by_name(tmp_path):
    """YAML 1.1 reads a bare `off` as the boolean `false` — the exact
    gotcha this chart's refusal names, so an operator who forgets to quote
    it is told why rather than meeting a bare type mismatch."""
    values = overlay("tls:\n  enabled: true\n  certSecret: x\n  clientAuth: off\n", tmp_path)
    result = render(CHART, "-f", str(values))
    assert result.returncode != 0, result.stdout
    assert "`tls.clientAuth` must be a quoted string" in result.stderr, result.stderr
    assert 'write `clientAuth: "off"`' in result.stderr, result.stderr


def test_client_auth_optional_is_refused_as_not_enforced_yet(tmp_path):
    result = render(
        CHART, "--set", "tls.enabled=true", "--set-string", "tls.clientAuth=optional"
    )
    assert result.returncode != 0, result.stdout
    assert "tls.clientAuth: optional" in result.stderr, result.stderr
    assert "not enforced yet" in result.stderr, result.stderr
    assert "B-U5" in result.stderr, result.stderr


def test_client_auth_required_is_refused_as_not_enforced_yet(tmp_path):
    result = render(
        CHART, "--set", "tls.enabled=true", "--set-string", "tls.clientAuth=required"
    )
    assert result.returncode != 0, result.stdout
    assert "not enforced yet" in result.stderr, result.stderr


def test_client_auth_unknown_value_is_refused_for_its_shape(tmp_path):
    result = render(
        CHART, "--set", "tls.enabled=true", "--set-string", "tls.clientAuth=sometimes"
    )
    assert result.returncode != 0, result.stdout
    assert '"off", "optional" or "required"' in result.stderr, result.stderr
    assert "not enforced yet" not in result.stderr, (
        "an unrecognised value was refused as if it were a disabled contract "
        f"value, instead of for its own shape: {result.stderr}"
    )


def test_client_ca_secret_set_renders_the_ca_file_env_and_mount(tmp_path):
    """BOTH gates must be satisfied: `clientAuth` present AND `clientCaSecret`
    truthy. This case states both."""
    result = render(
        CHART,
        "--set",
        "tls.enabled=true",
        "--set",
        "tls.certSecret=x",
        "--set-string",
        "tls.clientAuth=off",
        "--set",
        "tls.clientCaSecret=caller-ca",
        "--set",
        "tls.clientCaSecretKey=ca.crt",
    )
    assert result.returncode == 0, result.stderr
    assert "LISTEN_TLS_CLIENT_CA_FILE" in result.stdout
    assert "name: client-ca" in result.stdout
    assert "secretName: caller-ca" in result.stdout


def test_client_ca_secret_set_but_client_auth_absent_renders_nothing(tmp_path):
    """A CA bundle named with no `clientAuth` key describes a deployment
    that staged a Secret but never actually opted into client auth — the
    env, the mount and the volume must all stay absent, not just the env."""
    result = render(
        CHART,
        "--set",
        "tls.enabled=true",
        "--set",
        "tls.certSecret=x",
        "--set",
        "tls.clientCaSecret=caller-ca",
        "--set",
        "tls.clientCaSecretKey=ca.crt",
    )
    assert result.returncode == 0, result.stderr
    assert "LISTEN_TLS_CLIENT_CA_FILE" not in result.stdout
    assert "name: client-ca" not in result.stdout
    assert "secretName: caller-ca" not in result.stdout


def test_client_ca_secret_empty_string_renders_no_mount(tmp_path):
    """Truthiness, not `hasKey`: an explicit empty string is the unset
    state, the same rule every other optional Secret name in this chart
    follows, and must not render `secretName: ""`."""
    result = render(
        CHART,
        "--set",
        "tls.enabled=true",
        "--set",
        "tls.certSecret=x",
        "--set-string",
        "tls.clientAuth=off",
        "--set-string",
        "tls.clientCaSecret=",
    )
    assert result.returncode == 0, result.stderr
    assert "LISTEN_TLS_CLIENT_CA_FILE" not in result.stdout
    assert "name: client-ca" not in result.stdout
    assert 'secretName: ""' not in result.stdout


# ── K-1: both switches, both stated values, the unconditional render ──────


def test_both_switches_false_render_zero(tmp_path):
    result = render(
        CHART, "--set", "tls.enabled=false", "--set", "projectDb.tls.enabled=false"
    )
    assert result.returncode == 0, result.stderr
    assert 'name: LISTEN_TLS_ENABLED\n              value: "0"' in result.stdout
    assert 'name: PROJECT_DB_TLS_ENABLED\n              value: "0"' in result.stdout


def test_both_switches_true_render_one(tmp_path):
    result = render(
        CHART,
        "--set",
        "tls.enabled=true",
        "--set",
        "tls.certSecret=x",
        "--set",
        "projectDb.tls.enabled=true",
        "--set",
        "projectDb.tls.caSecret=x",
    )
    assert result.returncode == 0, result.stderr
    assert 'name: LISTEN_TLS_ENABLED\n              value: "1"' in result.stdout
    assert 'name: PROJECT_DB_TLS_ENABLED\n              value: "1"' in result.stdout


# ── MUTATION CHECKS (PROOF) ─────────────────────────────────────────────


def test_restoring_the_old_conditional_render_reddens_the_neutrality_case(tmp_path):
    """Revert `templates/deployment.yaml`'s `LISTEN_TLS_ENABLED` line to the
    pre-ADR-0845 `{{- if .Values.tls.enabled }}` shape and an explicit
    `tls.enabled=false` render stops naming the variable at all — the exact
    silent-default behaviour this change exists to remove.
    """
    chart_copy = tmp_path / "chart"
    shutil.copytree(CHART, chart_copy)
    template = chart_copy / "templates" / "deployment.yaml"
    text = template.read_text()
    unconditional = (
        '            - name: LISTEN_TLS_ENABLED\n'
        '              value: {{ ternary "1" "0" .Values.tls.enabled | quote }}\n'
    )
    reverted = (
        '            {{- if .Values.tls.enabled }}\n'
        '            - name: LISTEN_TLS_ENABLED\n'
        '              value: "1"\n'
        '            {{- end }}\n'
    )
    assert unconditional in text, "the unconditional render line moved or was reworded"
    template.write_text(text.replace(unconditional, reverted, 1))

    result = render(chart_copy, "--set", "tls.enabled=false")
    assert result.returncode == 0, result.stderr
    assert "LISTEN_TLS_ENABLED" not in result.stdout, (
        "reverting to the conditional render still names LISTEN_TLS_ENABLED at "
        "an explicit tls.enabled=false, so this mutation does not exercise the "
        "property ADR-0845 requires"
    )
