# Security policy

## Supported versions

Bouquet is pre-1.0. Security fixes go into the latest released version of
each package; older releases are not patched.

| Package | Supported |
| --- | --- |
| `bouquet` (crates.io) | latest `0.x` |
| `bouquet-bloom` (PyPI) | latest `0.x` |

## Reporting a vulnerability

Please **do not open a public issue** for a suspected vulnerability.

Report it privately through GitHub:
<https://github.com/klobucar/bouquet/security/advisories/new>

Include the affected package and version, what you observed, and a minimal
reproduction (inputs, feature flags, target architecture). If you can, say
whether it needs untrusted input to trigger.

You can expect an acknowledgement within a few days. This is a small
project maintained by one person, so there is no formal SLA, but confirmed
issues are fixed and released as a patch version with an advisory and a
`CHANGELOG.md` entry. Reporters are credited unless they ask otherwise.

## What counts

In scope:

- **Memory-unsafety or undefined behaviour** in safe API use. The crate
  contains `unsafe` in the hot loops (`get_unchecked` indexing and the NEON /
  AVX2 paths in `simd_blocked`); each block carries a `SAFETY` comment and
  the suite runs under Miri. A way to violate those invariants from safe code
  is a vulnerability.
- **Malicious serialized input** (`serde` feature): a blob that panics,
  aborts, loads into a filter that indexes out of bounds, or causes
  unbounded work or allocation. The deserializer is meant to reject these
  with a typed error.
- **Python bindings**: an input that crashes the interpreter (abort,
  segfault) instead of raising a Python exception.
- **Supply-chain issues** in the release process or workflows.

Out of scope, by design:

- **False positives.** A Bloom filter answers "maybe present" at a
  configured rate; that is the data structure, not a bug. A **false
  negative** (an inserted item reported absent) is a bug, but a correctness
  one; report it as a normal issue unless it is reachable from untrusted
  input.
- **Hash flooding when the seed is known.** The default hasher is keyed
  SipHash-1-3 with a random key, which resists collision attacks only while
  the key is secret. The key is part of the serialized form, so anyone who
  can read a persisted filter can craft colliding inputs. Treat persisted
  filters with the same confidentiality as the key. Using a fixed seed
  (`.seed(..)`) or a non-keyed custom hasher gives up this protection by
  choice.
- **Resource exhaustion from requesting a huge filter.** Sizing is the
  caller's decision. The fallible `try_*` constructors report failures as
  errors instead of aborting, and the Python bindings cap requests at 2^36
  bits (8 GiB), but a caller who asks for a very large filter will get one.
- Vulnerabilities in dev-dependencies that do not ship in the published
  crate or wheel.
