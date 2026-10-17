"""The ingress policy admits every named client, and still refuses an empty list.

`plans/the-d35-seeder.md` stage 3, case 7. The seeder joins `project`'s
callers: it dials `project` on 50052 to send `SyncProjects` (ADR-0936). The
TEMPLATE does not change for that, and neither does this chart's default
`clients: []`. The estate adds `seeder` beside `gateway` in its own values
(stage 7). This suite pins what that values change relies on:

- with `networkPolicy.clients: [gateway, seeder]`, the one ingress rule on
  50052 carries a `podSelector` for EACH, and nothing else;
- the empty-list refusal still fires (ADR-0664), for both shapes helm produces
  for "no clients": the chart default `[]` and `--set networkPolicy.clients={}`,
  which helm parses as a list holding one empty string.

THESE CASES PASS ON THE TEMPLATE AS IT STANDS, and that is intended. Nothing
in the template needed to change, so there is no red step; the cases exist so a
later edit that stops rendering a second client, or loosens the refusal, fails
here rather than on a live cluster as a silent timeout.

Run: python3 -m pytest scripts/tests/ -q
"""

from __future__ import annotations

from test_render_checks import CHART, objects, render

# The refusal's own opening words. OURS, so asserted exactly.
REFUSAL = "networkPolicy.enabled is true but networkPolicy.clients names no workload"


def ingress_policy(*arguments: str) -> dict:
    result = render(CHART, "--set", "networkPolicy.enabled=true", *arguments)
    assert result.returncode == 0, result.stderr
    policies = [o for o in objects(result.stdout) if o["kind"] == "NetworkPolicy"]
    assert len(policies) == 1, f"expected one NetworkPolicy, found {len(policies)}"
    return policies[0]


def grpc_clients(policy: dict) -> list[str]:
    """The `app` label of every source the 50052 rule admits, in order."""
    rules = [
        rule
        for rule in policy["spec"]["ingress"]
        if rule["ports"] == [{"protocol": "TCP", "port": 50052}]
    ]
    assert len(rules) == 1, f"expected one rule on 50052, found {len(rules)}"
    return [source["podSelector"]["matchLabels"]["app"] for source in rules[0]["from"]]


def test_the_gateway_and_the_seeder_are_both_admitted_on_50052() -> None:
    policy = ingress_policy(
        "--set", "networkPolicy.clients[0]=gateway",
        "--set", "networkPolicy.clients[1]=seeder",
    )
    assert grpc_clients(policy) == ["gateway", "seeder"]


def test_the_chart_default_still_names_no_client_and_refuses() -> None:
    result = render(CHART, "--set", "networkPolicy.enabled=true")
    assert result.returncode != 0, "an empty client list must refuse to render"
    assert REFUSAL in result.stderr, result.stderr


def test_a_set_empty_list_still_refuses() -> None:
    result = render(
        CHART, "--set", "networkPolicy.enabled=true", "--set", "networkPolicy.clients={}"
    )
    assert result.returncode != 0, "`clients={}` must refuse to render"
    assert REFUSAL in result.stderr, result.stderr
