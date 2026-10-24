//! The chart's `server.limits` block (ADR-1006) is held to the library that
//! reads it, and this is where that stops being a comment.
//!
//! `chart/values.schema.json` states a `minimum` and a `maximum` for each of the
//! four keys; `yadgar_lifecycle::serve_tls::ServerLimits` states the same ranges
//! as consts and refuses the boot outside them. Nothing held the two together:
//! bump the pin to a library that narrows a range and the chart would admit a
//! value the binary refuses at boot. This test reds on that bump.
//!
//! Every number is also pinned by a LITERAL (ADR-0599), so a change to a
//! constant and the schema together is still a deliberate edit here.

use std::ops::RangeInclusive;
use std::path::PathBuf;

use serde_json::Value;
use yadgar_lifecycle::serve_tls::ServerLimits;

/// (chart leaf, env variable, ADR-1006 default, the library's range, the range as literals).
type Row = (
    &'static str,
    &'static str,
    u64,
    RangeInclusive<u64>,
    (u64, u64),
);

fn rows() -> [Row; 4] {
    [
        (
            "maxConcurrentStreams",
            "LISTEN_MAX_CONCURRENT_STREAMS",
            256,
            ServerLimits::MAX_CONCURRENT_STREAMS,
            (1, 10_000),
        ),
        (
            "keepaliveIntervalSeconds",
            "LISTEN_KEEPALIVE_INTERVAL_SECS",
            30,
            ServerLimits::KEEPALIVE_INTERVAL_SECS,
            (1, 3_600),
        ),
        (
            "keepaliveTimeoutSeconds",
            "LISTEN_KEEPALIVE_TIMEOUT_SECS",
            10,
            ServerLimits::KEEPALIVE_TIMEOUT_SECS,
            (1, 600),
        ),
        (
            "maxHeaderListBytes",
            "LISTEN_MAX_HEADER_LIST_BYTES",
            16_384,
            ServerLimits::MAX_HEADER_LIST_BYTES,
            (1_024, 1_048_576),
        ),
    ]
}

fn read(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|why| panic!("{} must be readable: {why}", path.display()))
}

fn limits_schema() -> Value {
    let schema: Value = serde_json::from_str(&read("chart/values.schema.json"))
        .expect("chart/values.schema.json must be valid JSON");
    let block = schema["properties"]["server"]["properties"]["limits"].clone();
    assert!(block.is_object(), "the schema has no `server.limits` block");
    block
}

#[test]
fn the_schema_requires_each_key_as_an_integer_inside_the_library_range() {
    let block = limits_schema();
    for (leaf, _, _, range, (low, high)) in rows() {
        assert!(
            block["required"]
                .as_array()
                .is_some_and(|r| r.iter().any(|k| k == leaf)),
            "`server.limits.{leaf}` must be `required`: the binary has no default (ADR-1006)"
        );
        let property = &block["properties"][leaf];
        assert_eq!(property["type"], "integer", "{leaf}");
        let minimum = property["minimum"].as_u64().expect("`minimum` must be set");
        let maximum = property["maximum"].as_u64().expect("`maximum` must be set");
        assert_eq!(
            (minimum, maximum),
            (*range.start(), *range.end()),
            "{leaf}: the chart's range is not the one the pinned library refuses outside"
        );
        assert_eq!(
            (minimum, maximum),
            (low, high),
            "{leaf}: the library's range moved"
        );
    }
}

/// The defaults, read out of `server: limits:` in `chart/values.yaml`.
fn values_defaults() -> Vec<(String, u64)> {
    let values = read("chart/values.yaml");
    let mut in_server = false;
    let mut in_limits = false;
    let mut found = Vec::new();
    for line in values.lines() {
        if !line.starts_with(' ') && !line.starts_with('#') && !line.is_empty() {
            in_server = line.starts_with("server:");
            in_limits = false;
        } else if in_server && line.starts_with("  limits:") {
            in_limits = true;
        }
        if in_limits {
            if let Some((key, rest)) = line.trim().split_once(':') {
                let rest = rest.split('#').next().unwrap_or_default().trim();
                if let Ok(value) = rest.parse::<u64>() {
                    found.push((key.to_string(), value));
                }
            }
        }
    }
    found
}

#[test]
fn the_chart_defaults_are_the_adr_1006_numbers_and_inside_the_library_range() {
    let defaults = values_defaults();
    assert_eq!(
        defaults.len(),
        4,
        "expected exactly the four `server.limits` keys, got {defaults:?}"
    );
    for (leaf, _, want, range, _) in rows() {
        let (_, got) = defaults
            .iter()
            .find(|(key, _)| key == leaf)
            .unwrap_or_else(|| panic!("`server.limits.{leaf}` has no default in values.yaml"));
        assert_eq!(*got, want, "{leaf}: the ruled default (ADR-1006) moved");
        assert!(
            range.contains(got),
            "{leaf}: the chart default {got} is outside the range the binary accepts"
        );
    }
    let get = |leaf: &str| defaults.iter().find(|(k, _)| k == leaf).map(|(_, v)| *v);
    assert!(
        get("keepaliveTimeoutSeconds") < get("keepaliveIntervalSeconds"),
        "the default keepalive timeout must sit below the interval"
    );
}

/// The shipped defaults must also pass the library's own parser, cross-key rule
/// included, so the chart cannot ship a block the binary refuses to boot on.
#[test]
fn the_chart_defaults_parse_through_the_library() {
    let defaults = values_defaults();
    let lookup = |key: &str| {
        rows()
            .iter()
            .find(|(_, var, ..)| *var == key)
            .and_then(|(leaf, ..)| defaults.iter().find(|(k, _)| k == leaf))
            .map(|(_, v)| v.to_string())
    };
    ServerLimits::from_lookup("LISTEN", "server.limits", lookup)
        .expect("the chart defaults must be accepted by ServerLimits::from_lookup");
}

#[test]
fn the_deployment_renders_each_variable_from_its_chart_key() {
    let deployment = read("chart/templates/deployment.yaml");
    for (leaf, var, ..) in rows() {
        let at = deployment
            .find(&format!("- name: {var}\n"))
            .unwrap_or_else(|| panic!("deployment.yaml must render {var}"));
        let next: String = deployment[at..]
            .lines()
            .take(2)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            next.contains(&format!(".Values.server.limits.{leaf}")),
            "{var} is not rendered from `server.limits.{leaf}`: {next}"
        );
    }
}
