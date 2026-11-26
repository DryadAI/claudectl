# Precision DRUID proxy policy proposal

This change adds a dormant policy contract for the approved local proxy route.
The [proposed example](../examples/druid-precision-standing-policy-proxy-proposed.json)
has an inactive status. It is not an activation packet or a credential source.

The protected policy accepts only `sandbox.network = root-owned-loopback-proxy`
with `proxy_url = http://127.0.0.1:18443`, the single subscription route label,
and an explicit API-key prohibition. Other restricted credentials and operations
remain denied. Offline policy continues to require its original route and no
proxy URL. A permit carries the authorized route to the headless adapter, which
refuses a mismatched worker configuration.

Immediately before a proxy-mode launch, a root supervisor must verify the
protected root-owned firewall loader, its exact rule set, and both required
system services. An unavailable or changed boundary blocks launch. The worker
transport separately fixes the URL and proxy environment. The provider is still
unregistered in the durable supervisor and CLI.

On Precision, the firewall and proxy started in order after the 2026-11-26
reboot. The owner ran the root exact-rule verifier and the worker-context probes;
the probes passed. This is host boundary evidence, not an authenticated worker
inference test or a completed graph.

Before activating the proposed policy, establish how the owner's first-party
subscription can authenticate inside an isolated ephemeral home without copying
account tokens into DRUID, relaxing mounts, or allowing API fallback. A working
proxy tunnel alone does not prove this. Anthropic's
[authentication policy](https://code.claude.com/docs/en/legal-and-compliance)
distinguishes end-user sign-in to unmodified Claude Code from integrations that
collect or intermediate subscription credentials. Review those conditions for
the actual deployment. The positive `max_budget_usd` field is the Claude CLI's
per-run counter, not authorization to spend on a paid API; the transport forbids
API-key and base-URL variables. Do not activate on that counter alone.

After authentication is resolved, the remaining gates are trusted worker
configuration, full resource limits, independent verifier, cancellation and
recovery evidence, and one disposable repository graph. Keep the current
protected policy and provider registration unchanged until those checks pass.
