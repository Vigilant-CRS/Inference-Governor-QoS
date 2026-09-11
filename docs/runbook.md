# Runbook

What to do when something is wrong. Written for the person who is on call at
three in the morning, not for the person who wrote the code.

Every symptom below is one the system reports about itself. If a symptom is not
in this list and no metric shows it, that is a gap in the software — please open
an issue.

## The two health endpoints and what they mean

| Endpoint | Answers | Use it for |
|---|---|---|
| `/healthz` | Is the process alive? | Restart decisions |
| `/readyz` | Can the process achieve anything? | Load balancer, rollout gate |

The distinction matters. A governor whose backend has vanished is perfectly
alive and completely useless. `/healthz` stays green, `/readyz` goes red.
**Never** wire a restart to `/readyz` — restarting will not bring the backend
back, and a restart loop destroys the lease state that recovery needs.

## Symptoms

### `/readyz` red: "N von M Slots in Quarantaene"

**What happened.** Backend calls timed out, and no proof of completion has
arrived. The slot credits stay held on purpose: the GPU may still be computing,
and returning a credit would put a second inference on the same unit.

**What to do.**

1. Check whether the backend is answering at all:
   `curl -sf localhost:8000/v2/health/ready`.
2. If it answers, the reconciliation is running and the credits come back on
   their own. Watch `vig_execution_reconciled_total` climb.
3. If it does not answer, the backend needs attention. The governor recovers
   without intervention as soon as the backend replies.

**What not to do.** Do not restart the governor to "clear" the quarantine. The
quarantine is knowledge, not a stuck state — restarting throws that knowledge
away, and the new process starts into an occupancy it knows nothing about.

### `/readyz` red: "N Transportfehler seit dem letzten Erfolg"

**What happened.** The connection to the backend cannot be established. Unlike
the case above, no slot is stuck — the calls return immediately with an error.

**What to do.** This is almost always the backend being down, the wrong
endpoint, or a network policy. `vig doctor -c your.yaml` names which. One
successful call resets the counter and the endpoint goes green again.

### `vig_reconcile_baseline_missing` above zero

**What happened.** The governor could not read the backend's completion
counter at startup. Without that baseline there is no counter-based proof of
completion, so an aborted call holds its slot credit until the backend
demonstrably restarts.

Why a baseline is needed at all: Triton's statistics counter runs for the
lifetime of the *Triton* process, and that process normally outlives the
governor. After a governor restart the counter already stands at thousands of
completions — "the backend reports at least as many completions as we
dispatched" would be true on the very first request, and the reconciliation
would free a credit while the compute unit is still busy.

**What to do.** Restart the governor while the backend is reachable. The
baseline is only accepted before the first dispatch to a model: a baseline read
later could already include our own completed inferences, which would make it
too high and the target unreachable — a credit held forever is not the safe
side, it is a different way of being broken.

**Why this is not automatic.** The governor cannot tell whether a counter it
reads late already contains its own work. Refusing the late baseline and saying
so is the only honest option.

### `vig_best_effort_starved_total` climbing

**What happened.** Protected streams leave no headroom. Background work never
starts. This is by design (ADR-0012) and it is still worth knowing about.

**What to do.** Either accept it, or reduce protected load, or decompose the
background model into cooperative quanta (`cooperative:` in the model config).
`vig doctor` warns about this before you start, from the configured profiles
alone.

### `vig_weakly_hard_violated{model="…"} 1`

**What happened.** A stream missed more consumer cycles in its window than its
contract allows. The window is **not** reset by the violation, so this stays 1
until a full window has passed cleanly.

**What to do.** Check `vig_weakly_hard_misses_left` for the same model — if it
has been at 0 for a while, the contract is not achievable on this hardware
under this load. That is a capacity finding, not a bug. `vig doctor` computes
the protected serialised utilisation and says whether it fits at all.

### `vig_longest_gap_us` jumps

**What happened.** A stream went without a usable result for that long. A high
coverage number can hide this: ten scattered misses and one block of ten give
the same rate and completely different consequences for a controller.

**What to do.** Correlate with `vig_weakly_hard_misses_left` and with the
hardware state — a thermal limit or a clock drop shows up here first.

### `doctor` says "GPU 0: gedrosselt … Grund [SwPowerCap]"

**What happened.** The card is not running at full clock. Nothing is broken,
but every profile measured in this state describes the card **in this state**.

**What to do.** If this is the production state, measure the profiles here —
that is the honest thing. If it is not, find out why the card is limited
(power mode, thermals, a laptop on battery) before measuring.

### Profile mismatch warnings at startup

**What happened.** The stored profile belongs to a different environment. The
governor names the field: artifact digest, runtime, device, driver,
partitioning.

**What to do.** Re-measure with `vig calibrate`. Until then the affected model
is planned with a raised margin (ADR-0016) — less throughput, no wrong
promises. The governor does not refuse to start: on a robot, a refused service
means perception falls out entirely.

## Watching a long run

```bash
soak measured.yaml > run/console.log 2>&1 &
tools/run-watch.sh run $! 480 60
```

The watcher takes a **PID**, not a process pattern. `pgrep -f` searches the
full command line, so a pattern passed as an argument matches the watcher's own
process — and the watcher then waits for itself forever. That failure is silent:
a watch that says nothing looks exactly like a run still in progress. It cost us
four hours once.

It reports three terminal states, not one: finished with the run's own marker,
gone without a marker, and error or panic lines in the log. If the run crashed
right now, a line would come.

## Draining and restarting

```bash
# Graceful: waiting work is answered, running calls finish.
kill -TERM <pid>
```

`drain` returns `false` if it could not finish within the deadline. That is
**not** a bug to be ignored: it means a backend call is still outstanding and
the compute unit may still be busy. Wait, or accept that the next process
starts into an occupancy it does not know about.

The shutdown deadline is bounded. A drain that never ends is not a drain.

## Update and rollback

1. **Read the current lease state** before stopping: `curl -s
   localhost:9090/metrics | grep vig_quarantined_slots`. Non-zero means a
   compute unit may be busy; wait for it.
1. **Start the new process while the backend is reachable.** It reads the
   backend's completion counter once at startup as the reconciliation baseline,
   and it only accepts that baseline before the first dispatch. Check
   `vig_reconcile_baseline_missing 0` before sending traffic — otherwise an
   aborted call will hold its slot credit for the life of the process.
2. **Drain, then stop.** Never `SIGKILL` a governor with held credits — the
   next process cannot know what the previous one was holding.
3. **Start the new version and check `/readyz`** before sending traffic.
4. **Rollback** is the previous binary plus the previous configuration. Both
   together: a profile from the new version may carry a manifest the old one
   does not understand, and it will be refused (which is correct).

Profiles are not portable between versions of the *backend*. If the update
includes a new Triton or a new driver, re-measure.

## Where the limits are

These are engineering limits, stated so that nobody discovers them in
production:

- **One execution unit.** The slot set models one GPU. Multi-GPU is not
  supported, and MIG changes the model in a way the current code does not
  represent.
- **Not functionally safe.** No claim of ISO 26262 / IEC 61508 conformance.
  Do not make a safety function depend on this governor.
- **Numbers are per hardware configuration.** The logic is portable, the
  numbers are not. See [hardware qualification](hardware-qualification.md).
- **Every request costs time.** On the shared-memory path at most 300 µs at
  the median, independent of the frame size; the gRPC copy path is not a
  supported path for camera frames. The budgets, and the two checks to run on
  the release machine before a release: [datapath budgets](datapath-budgets.md).
- **Contracts are never measured.** What has to be fresh is a statement about
  what the robot needs; only the operator can make it. A system that invents
  its own deadlines cannot be held to them.

## Security

What the governor protects, the secure-deployment checklist and the accepted
residual risks are in [security.md](security.md). Two runtime signals:

* `vig serve` exits at startup with "ausserhalb von Loopback" — the listen
  address is not loopback and neither `client_ca` nor `token_file` is set. Set
  one; `--insecure-open` is only for a container port published on the host's
  loopback.
* A client gets `PERMISSION_DENIED` from model load/unload or from
  unregistering all shared-memory regions — those need a token from
  `backend.security.admin_token_file`; without that file they are closed.

## Support boundaries

GitHub issues for bugs and questions about the software. For a commercial
licence, a supervised pilot, or anything that needs a person rather than a
tracker: **info@vigilant-crs.de** ([who we are](../IMPRINT.md)).

There is no 24/7 response commitment in the open-source scope. A pilot
agreement can define one.
