"""`tls.enabled` / `projectDb.tls.enabled` HAVE NO DEFAULT (ADR-0845, ledger 965 K-1).

The binary refuses the boot on anything but exactly "1" or "0" for
`LISTEN_TLS_ENABLED` / `PROJECT_DB_TLS_ENABLED` (`src/serve.rs`,
`src/upstream.rs`). This chart renders both UNCONDITIONALLY, as
`ternary "1" "0" <raw value>`, so the shape has to be refused before that
line runs rather than left to render whatever `ternary` makes of a bad
value.

TWO LAYERS CATCH DIFFERENT SHAPES, MEASURED RATHER THAN ASSUMED (K-1).
`values.schema.json`'s `required` + `type: boolean` on the `enabled` leaf
catches: the key missing entirely, a null value (helm deletes a
null-valued key the chart declares, which is indistinguishable from
missing), and any non-boolean scalar — ALL OF THESE REFUSE BEFORE THE
TEMPLATE RUNS AT ALL, with helm's OWN schema-validation wording, which
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
inside it. That guard's refusal is OURS, so it is asserted exactly.

Run: python3 -m pytest scripts/tests/ -q
"""

from __future__ import annotations

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


def test_tls_enabled_absent_refuses_at_the_schema(tmp_path):
    """Deleting `tls.enabled` (null overwrites the chart's own `false`)."""
    values = overlay("tls:\n  enabled: null\n", tmp_path)
    result = render(CHART, "-f", str(values))
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


def test_tls_enabled_false_still_renders_the_variable(tmp_path):
    """UNCONDITIONAL means the chart's own default — `false` — renders the
    variable too, which an `if` never did."""
    result = render(CHART)
    assert result.returncode == 0, result.stderr
    assert 'name: LISTEN_TLS_ENABLED\n              value: "0"' in result.stdout


# ── `projectDb.tls` (the DIAL) — the identical three shapes ───────────────


def test_projectdb_tls_enabled_absent_refuses_at_the_schema(tmp_path):
    values = overlay("projectDb:\n  tls:\n    enabled: null\n", tmp_path)
    result = render(CHART, "-f", str(values))
    assert result.returncode != 0, result.stdout
    assert SCHEMA_WRAPPER in result.stderr, result.stderr
    assert "projectDb" in result.stderr and "enabled" in result.stderr, result.stderr


def test_projectdb_tls_enabled_wrong_type_refuses_at_the_schema(tmp_path):
    values = overlay('projectDb:\n  tls:\n    enabled: "true"\n', tmp_path)
    result = render(CHART, "-f", str(values))
    assert result.returncode != 0, result.stdout
    assert SCHEMA_WRAPPER in result.stderr, result.stderr


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


def test_projectdb_tls_enabled_false_still_renders_the_variable(tmp_path):
    result = render(CHART)
    assert result.returncode == 0, result.stderr
    assert 'name: PROJECT_DB_TLS_ENABLED\n              value: "0"' in result.stdout


# ── `tls.clientAuth` (B-U5E, folded expand) ────────────────────────────────


def test_client_auth_absent_is_render_neutral(tmp_path):
    """Unset renders nothing for it — the whole point of an expand (K-8)."""
    result = render(CHART)
    assert result.returncode == 0, result.stderr
    assert "LISTEN_TLS_CLIENT_AUTH" not in result.stdout
    assert "LISTEN_TLS_CLIENT_CA_FILE" not in result.stdout


def test_client_auth_off_is_the_only_accepted_value(tmp_path):
    result = render(CHART, "--set", "tls.enabled=true", "--set", "tls.certSecret=x", "--set-string", "tls.clientAuth=off")
    assert result.returncode == 0, result.stderr
    assert 'name: LISTEN_TLS_CLIENT_AUTH\n              value: "off"' in result.stdout


def test_client_auth_optional_is_refused_as_not_enforced_yet(tmp_path):
    result = render(CHART, "--set-string", "tls.clientAuth=optional")
    assert result.returncode != 0, result.stdout
    assert "tls.clientAuth: optional" in result.stderr, result.stderr
    assert "not enforced yet" in result.stderr, result.stderr
    assert "B-U5" in result.stderr, result.stderr


def test_client_auth_required_is_refused_as_not_enforced_yet(tmp_path):
    result = render(CHART, "--set-string", "tls.clientAuth=required")
    assert result.returncode != 0, result.stdout
    assert "not enforced yet" in result.stderr, result.stderr


def test_client_auth_unknown_value_is_refused_for_its_shape(tmp_path):
    result = render(CHART, "--set-string", "tls.clientAuth=sometimes")
    assert result.returncode != 0, result.stdout
    assert '"off", "optional" or "required"' in result.stderr, result.stderr
    assert "not enforced yet" not in result.stderr, (
        "an unrecognised value was refused as if it were a disabled contract "
        f"value, instead of for its own shape: {result.stderr}"
    )


def test_client_ca_secret_set_renders_the_ca_file_env_and_mount(tmp_path):
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
    assert "LISTEN_TLS_CLIENT_CA_FILE" in result.stdout
    assert "name: client-ca" in result.stdout
    assert "secretName: caller-ca" in result.stdout


# ── MUTATION CHECKS (PROOF) ─────────────────────────────────────────────


def test_restoring_the_old_conditional_render_reddens_the_neutrality_case(tmp_path):
    """Revert `templates/deployment.yaml`'s `LISTEN_TLS_ENABLED` line to the
    pre-ADR-0845 `{{- if .Values.tls.enabled }}` shape and the default render
    (`tls.enabled: false`) stops naming the variable at all — the exact
    silent-default behaviour this change exists to remove.
    """
    import shutil

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

    result = render(chart_copy)
    assert result.returncode == 0, result.stderr
    assert "LISTEN_TLS_ENABLED" not in result.stdout, (
        "reverting to the conditional render still names LISTEN_TLS_ENABLED at "
        "the chart's own tls.enabled=false default, so this mutation does not "
        "exercise the property ADR-0845 requires"
    )


def test_dropping_the_schema_required_still_reddens_but_the_message_may_degrade(tmp_path):
    """Drop `values.schema.json`'s `required: ["enabled"]` on `tls`. The
    template's own `hasKey .Values.tls "enabled"` arm is the second layer —
    it still refuses, but ONLY the red/green outcome is asserted here, not a
    specific message: depending on the exact shape, this can also surface as
    a raw Sprig type error from the now-unconditional `ternary` line in
    `templates/deployment.yaml`, not this chart's own sentence (coordinator
    note, measured on project-db#54). Message QUALITY is not this test's
    job; `test_tls_block_null_refuses_at_the_template_guard` above is where a
    specific sentence is pinned, for a shape the schema never reaches at
    all.
    """
    import json
    import shutil

    chart_copy = tmp_path / "chart"
    shutil.copytree(CHART, chart_copy)
    schema_path = chart_copy / "values.schema.json"
    schema = json.loads(schema_path.read_text())
    schema["properties"]["tls"].pop("required", None)
    schema_path.write_text(json.dumps(schema))

    values = overlay("tls:\n  enabled: null\n", tmp_path / "values")
    result = render(chart_copy, "-f", str(values))
    assert result.returncode != 0, (
        "dropping the schema's `required` on `tls.enabled` let an absent "
        f"value through with no refusal at all: {result.stdout}"
    )
