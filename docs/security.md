# Security

What Vigilant protects, what it deliberately does not, and how to deploy it so
that the difference does not matter. The findings of the security review of
2026-09-11 (H = high, M = medium, N = low, I = informational) and what became
of each are in the [table at the end](#review-findings-and-fixes).

## Threat model

The governor controls a whole GPU: which model runs, when, and which request
is dropped. Anyone who can call it can take that control. Anyone who can call
the **backend** directly bypasses it altogether.

The design case is **one device, one operator, one closed network**: a robot or
an edge box on which the governor, Triton and the clients run on the same host.
The defaults are chosen for that case, and they stay safe when someone starts
the shipped files unchanged:

| Default | Why |
|---|---|
| `vig serve` listens on `127.0.0.1:9001`, metrics on `127.0.0.1:9090` | nothing is reachable from the network until someone decides so |
| `vig serve` refuses a non-loopback address unless mTLS (`client_ca`) or tokens (`token_file`) check the caller | TLS alone encrypts but checks nobody; the explicit way past is `--insecure-open`, meant for a container port published on the host's loopback |
| Administration endpoints are closed until `backend.security.admin_token_file` is set | one `RepositoryModelUnload` would otherwise take the protected model away |
| The Compose file publishes nothing but `127.0.0.1:9001` and `127.0.0.1:9090`; Triton has no published port | the backend is never reachable past the governor |
| `trust: open` | benchmarks and existing clients register shared memory directly with Triton; `strict` is the mode for anything beyond one operator (see I1) |

Out of scope: a hostile process with the same user or root on the host, a
compromised Triton, and physical access. The governor never reads tensor
payloads (ADR-0003); it cannot tell a malicious input from a good one.

## What is checked

**Identity.** With `token_file` or `client_ca`, every gRPC request is checked,
by an interceptor that runs before the request body is decoded — an
unauthenticated client never costs a 64 MiB message buffer. Tokens are at least
16 characters. A token may carry a name (`robot:<token>`); only named tokens can
send application hints, and the hint authority is derived from the name, so
nothing derived from a secret is ever logged. An admin token also
authenticates ordinary requests.

**Transport limits** (`backend.security.transport`, all > 0, defaults shown):

```yaml
backend:
  security:
    transport:
      max_concurrent_streams: 256       # HTTP/2 streams per connection
      concurrency_per_connection: 64    # requests served at once per connection
      request_timeout_ms: 600000        # generous: a split generative job takes many quanta
      keepalive_interval_ms: 30000      # HTTP/2 and TCP keepalive
      keepalive_timeout_ms: 10000
```

The metrics endpoint accepts at most 16 connections at a time.

**Shared memory.** Registrations through the governor must name a key starting
with `shm_key_prefix` (default `/vig_`, no further `/`), a non-zero size whose
end does not overflow, and a name no other caller owns. At most
`max_shm_regions` (default 256) exist. Only the owner unregisters a region;
unregistering *all* regions (empty name) needs an admin token. Under
`trust: strict` an inference may only name regions its own caller registered
through the governor, in inputs and outputs alike, and unknown regions cannot
be unregistered. Under `strict` the **segment** behind a key belongs to the
caller who registered it first through the governor: another caller cannot
register the same key under a different name, whatever offset and size it
names, and `SystemSharedMemoryStatus` lists only the caller's own regions (an
admin token sees all). A registration reserves its name, slot and segment
before it reaches Triton and is booked only when Triton confirms, so
concurrent registrations cannot exceed `max_shm_regions`; a second
registration or unregistration of a name whose call is still running is
answered with `ABORTED`.

After a request has been sent, a lost connection or response leaves its outcome
unknown. The reservation remains held, including its segment ownership and
capacity, until the backend state can be reconciled. A definitive rejection
such as `INVALID_ARGUMENT` releases it. Automatic reconciliation of these
uncertain registrations is not implemented: affected names remain `ABORTED`,
and uncertain registrations still consume the region limit. Recovery requires
controlled reconciliation or reinitialization of both backend and governor
with clients stopped; merely retrying under another name must not bypass the
reservation. This also applies to an uncertain unregister-all operation.

**Payload budget.** `max_inflight_mib` counts every request that carries bytes,
including unconfigured models passed through in open mode.

**Dependency graph.** Request ids given with `vig_capture_id` are namespaced per
caller: a caller cannot name another caller's request as a parent, and cannot
reuse an id whose request is still open (`vig-reason: duplicate_id`). An
authenticated caller holds at most half as many **open** requests as the graph
has room for (`graph_quota`); finished results kept for later consumers do not
count, they are released first under pressure. A full graph answers
`RESOURCE_EXHAUSTED` with `vig-reason: graph_full`.

**Hints.** A hint whose lifetime exceeds `hints.max_ttl_ms` (default 60 s, at
most one hour) is dropped, not shortened — shortened, it would mean something other than the
application believes.

**Logs.** An implausible client timestamp is counted on every request and
logged at most once per 10 s.

**Liveness.** gRPC `ServerLive` is answered by the governor itself and
`ServerReady` from its own state (active backend probe and quarantine, the same
decision as `/readyz`); neither calls Triton.

## Secure deployment checklist

For anything beyond one operator on one host:

1. `trust: strict` — no bypassing the governor, no self-promotion, only own
   shared-memory regions.
2. mTLS (`tls_cert`, `tls_key`, `client_ca`) or `token_file` with named tokens.
   `vig doctor` reports `trust: strict` without either as **not ready**.
3. `admin_token_file` only if something must load or unload models, with a
   token that no inference client holds.
4. Keep the metrics port on loopback (it is not authenticated) or put an
   authenticating proxy in front.
5. Keep Triton's gRPC and HTTP ports unpublished; clients reach it only
   through the governor.
6. Verify release artifacts as described in [releases.md](releases.md) — the
   signing identity is pinned to the release workflow on a version tag.
7. Run `vig doctor` after every configuration change.

## Accepted residual risks

| Risk | Why it stays | Mitigation |
|---|---|---|
| Triton runs with `ipc: host` and as root in the Compose file | shared memory with clients on the host needs the host IPC namespace; dropping `DAC_OVERRIDE` would stop root in the container from reading client-owned segments | `no-new-privileges`; no published ports; the governor's key prefix limits what is registered **through it** |
| Shared memory registered **directly** at Triton (bypassing the governor) is not visible to the governor | benchmarks and existing clients do exactly this; in open mode it is allowed | `trust: strict` refuses any inference that names such a region |
| `curl` is in the runtime image | the container health check uses it | image is otherwise minimal; runs read-only without capabilities |
| In `trust: open`, `SystemSharedMemoryStatus` lists the regions of all callers | pass-through of the backend's answer; names carry no payload | `strict` lists only the caller's own regions; pick non-telling names |
| Under `strict`, a caller who registers another caller's key **before** its owner does holds that segment | the governor sees keys, not which process created a segment; binding keys to identities needs operator-assigned namespaces | the owner's own registration then fails with `PERMISSION_DENIED`, so the takeover is not silent; use keys that cannot be guessed |
| The metrics port is unauthenticated and has no idle timeout | Prometheus scrapes are unauthenticated by convention | loopback by default; at most 16 connections |
| The implausible-timestamp counter is not exported as a metric | only in the log line (`total=`) | the log line is rate-limited and carries the total |
| Governor → Triton is plaintext gRPC (I3) | loopback or the Compose network | for a remote backend, use a private network or a TLS-terminating tunnel |
| Token lookup is a hash-map lookup, not a constant-time compare (I3) | keyed SipHash with a random key gives no practical timing channel | tokens ≥ 16 characters |
| Unnamed tokens authenticate but cannot send hints | the authority used to be derived from the token itself (N1) | name the token (`name:token`) and use the authority `vig serve` prints for the name |

## Review findings and fixes

| Finding | Fix | Test |
|---|---|---|
| **H1** Quickstart Compose published Triton to the network, past the governor | Triton has no published port; `vig` ports bound to `127.0.0.1`; `vig` read-only, `cap_drop: ALL`, `no-new-privileges`; `.dockerignore` keeps tokens, keys and local configs out of the build context | Compose file review; `serve::tests::loopback_needs_no_identity_check` and siblings for the refusal outside loopback |
| **H2** Shared-memory keys passed through unchecked | key prefix, extent and ownership checks on every registration through the governor; under `strict` inferences may only name own regions | `security::shm_registration_is_confined_to_its_prefix_owner_and_bound`, `security::strict_mode_only_lets_a_caller_name_its_own_regions`, `security::strict_mode_refuses_a_foreign_segment_under_another_name`, `security::strict_status_lists_only_own_regions`, `shm::tests::a_segment_belongs_to_its_registrant_when_exclusive`, `shm::tests::keys_must_carry_the_prefix_and_nothing_else`, `shm::tests::extents_must_not_overflow`, `shm::tests::a_name_belongs_to_its_registrant`, `service::tests::region_references_are_found_in_inputs_and_outputs` |
| **H3** Admin endpoints open to every token holder | closed in every mode until `admin_token_file`; then only its tokens | `security::admin_endpoints_are_closed_until_an_admin_token_is_presented` |
| **M1** Auth after full decode; no transport limits | `Gate` interceptor before decoding; `backend.security.transport` limits | `security::the_gate_refuses_before_anything_is_decoded`, `auth::tests::the_gate_rejects_before_the_service_sees_anything`, config tests `the_security_limits_*` |
| **M2** Pass-through bypassed the payload budget | pass-through reserves budget like configured work | `security::passthrough_is_bounded_by_the_payload_budget` |
| **M3** TLS without client CA and without tokens counted as protected | `serve` refuses non-loopback without identity check (`--insecure-open` to override); `doctor` warns, and fails `strict` without identity | `serve::tests::an_open_endpoint_without_identity_is_refused_even_with_tls`, `serve::tests::identity_or_the_explicit_flag_opens_it`, `doctor::tests::tls_alone_is_not_an_identity_check` |
| **M4** Empty-name unregister removed every client's regions | needs an admin token; single names only by their owner | `security::shm_registration_is_confined_to_its_prefix_owner_and_bound`, `shm::tests::an_empty_name_unregisters_everything` |
| **M5** Release supply chain | every action pinned by commit SHA; `permissions: contents: read` with write only in the publishing job; tool versions pinned; Dependabot for actions and base images; verification identity pinned to `release.yml` on a `v*` tag | workflow review |
| **N1** Unsalted token hash in the log | hint authority from the token's name; only names are logged; minimum token length 16 | `auth::tests::the_hint_authority_comes_from_the_label_not_the_secret`, `auth::tests::a_short_token_is_refused`, `auth::tests::duplicate_labels_and_tokens_are_refused` |
| **N2** DAG ids global and silently overwritten | ids namespaced per caller; open ids not reusable; per-identity quota | `security::request_ids_are_namespaced_per_caller`, `security::an_open_request_id_cannot_be_used_twice`, `security::one_identity_cannot_fill_the_graph_alone` |
| **N3** Health endpoints unauthenticated, contrary to the docs | gRPC live/ready authorised and answered locally; docs now say "every gRPC request" and name the unauthenticated metrics port | `security::liveness_is_answered_locally` |
| **N4** Metrics server without limits | at most 16 concurrent connections | `metrics_listener::the_metrics_listener_holds_at_most_its_limit` |
| **N5** Clients could flood the log | warning rate-limited to one per 10 s, with a running total | `security::implausible_client_timestamps_are_counted_not_each_logged` |
| **N6** Hint TTL without upper bound | `hints.max_ttl_ms`, default 60 s; longer hints are dropped | `service::tests::a_hint_longer_than_the_limit_is_dropped`, config tests |
| **N7** Container hardening | base images pinned by digest; `ShmRegistry` bounded (`max_shm_regions`), also for concurrent registrations (slots reserved before the backend call); Triton `no-new-privileges`; residuals listed above | `shm::tests::the_registry_is_bounded`, `shm::tests::a_reservation_holds_its_place_until_it_is_settled`, `shm::tests::calls_for_one_name_do_not_interleave`, `security::parallel_registrations_respect_the_region_limit`, config tests |
| **I1** `trust: open` default plus Compose binding made an unsafe quickstart | accepted for `open` (benchmarks and pass-through depend on it); the quickstart is no longer unsafe because of H1 and M3 | — |
| **I2** `ci.yml` without `permissions` | `permissions: contents: read` | workflow review |
| **I3** Token compare timing; plaintext gRPC to Triton | accepted, see residual risks | — |

### Not a security finding: NV-17

After 256 captures the gateway refused every frame. The graph holds 256 nodes,
and no path ever moved a node to a terminal state, so nothing was ever
collected. Every terminal path — completion, stale or superseded drop,
rejection, cancellation, backend failure, drain — now moves the node to its
terminal state; a completed node is kept for its model's `max_age` (clamped to
100 ms – 5 s, default 1 s) so later consumers of the same capture still find
it, and released early under pressure. DAG rejections carry `vig-reason`
(`graph_full`, `graph_quota`, `capture_mismatch`, `unknown_parent`,
`duplicate_id`, …). Tests: `security::thousands_of_captures_are_all_delivered`
(2,000 captures, 4,000 requests), `security::a_parent_outlives_its_first_consumer`,
`security::a_full_graph_answers_graph_full`.
