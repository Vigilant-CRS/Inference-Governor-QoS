# Releases, versioning and updates

## What a release is

What changed in each version: [CHANGELOG.md](../CHANGELOG.md).

A tagged commit, built by CI, signed with Sigstore, and shipped with a bill of
materials. Nothing is built on a developer machine and uploaded.

Every artifact carries:

| File | What it is |
|---|---|
| `vig-<version>-<target>.tar.gz` | the binary, plus `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.md` |
| `.sha256` | checksum |
| `.sig` + `.pem` | cosign signature and certificate — keyless, bound to the GitHub identity of the workflow |
| `*.cdx.json` | CycloneDX SBOM |

The binary itself also embeds its dependency list (`cargo auditable`), so you
can check it for known vulnerabilities **without** this repository:

```bash
cargo audit bin vig
```

## Verifying what you downloaded

```bash
cosign verify-blob \
  --certificate      vig-0.1.0-x86_64-unknown-linux-gnu.tar.gz.pem \
  --signature        vig-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sig \
  --certificate-identity-regexp '^https://github\.com/Vigilant-CRS/Inference-Governor-QoS/\.github/workflows/release\.yml@refs/tags/v[0-9].*$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  vig-0.1.0-x86_64-unknown-linux-gnu.tar.gz

sha256sum -c vig-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

The identity is pinned to the release workflow **on a version tag**. A
signature produced by any other workflow in the repository, or by the release
workflow on a branch, does not verify — so a compromised test job cannot mint
a release signature.

There is no private signing key. We could not leak one, and you do not have to
trust us to protect it — the signature binds the artifact to the workflow run
that produced it, and that run is public.

**The two commands do different things, and only one of them is security.**
`cosign verify-blob` proves where the artifact came from. `sha256sum -c` catches
a corrupted download — the checksum file is not signed, so an attacker who could
replace the archive could replace the checksum with it. Run both; trust the
first.

## Targets

| Target | Built | Tested |
|---|---|---|
| `x86_64-unknown-linux-gnu` | yes | full suite, plus measurements on real hardware |
| `aarch64-unknown-linux-gnu` | yes | scheduling core under emulation — **no hardware measurements** |

See [hardware-qualification.md](hardware-qualification.md) for what "no
hardware measurements" means in practice, and what you have to run before
trusting a platform we have not measured.

## Versioning

Semantic versioning, with the interfaces spelled out because "the API" is
ambiguous for a piece of infrastructure:

| Surface | Rule |
|---|---|
| gRPC service | Open Inference Protocol. We do not change it; we implement it. |
| `vig_*` request parameters | A new one is a minor version. Removing or changing the meaning of one is major. |
| `vig-reason` values | Adding a reason is minor. Clients must tolerate unknown ones. |
| Configuration schema | A new optional field is minor. A new required field, a changed default that alters behaviour, or a removed field is **major**. |
| Prometheus metric names | Adding is minor. Renaming or removing is major. |
| Runtime profiles | Not stable across versions. Re-run `vig calibrate` after an upgrade. |
| Rust crate APIs | Not public. The crates are published for building the binary, not for embedding. |

Before 1.0, minor versions may still break the configuration schema. We will
say so in the release notes, and `vig doctor` will name the field.

## Upgrading

```bash
vig doctor -c your.yaml        # 1. does the config still parse and validate?
vig calibrate -c your.yaml -o measured.yaml   # 2. re-measure; profiles do not survive upgrades
# 3. deploy, watch vig_protected_deadline_misses_total and vig_quarantined_slots
```

Step 2 is not optional and not a formality. A profile is a measurement under
conditions — a new backend version, a new driver, a new container image all
change the conditions. The governor detects the mismatch by fingerprint and
plans more conservatively, which costs throughput until you re-measure.

**Rolling back** is a matter of putting the previous binary back and restoring
the previous configuration. State lives in neither: the governor learns its
margins at runtime and starts from the configured values every time.

## Support policy

Pre-1.0: the newest release only. There are no backports, and we would rather
say that than imply a maintenance window we do not staff.

Security reports: [SECURITY.md](../SECURITY.md) — to info@vigilant-crs.de, not as
a public issue.

Commercial licences, pilots and everything else that needs a human:
[IMPRINT.md](../IMPRINT.md).
