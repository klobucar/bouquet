# Contributing to Bouquet

Thanks for your interest. Bug reports, benchmarks, docs fixes and focused
pull requests are all welcome. For anything larger than a small fix, please
open an issue first so we can agree on the approach before you spend time.

Security problems go through [SECURITY.md](SECURITY.md), not public issues.

## Layout

| Path | What it is |
| --- | --- |
| `crates/bouquet` | The Rust crate (published to crates.io) |
| `bouquet-py` | PyO3 bindings, published to PyPI as `bouquet-bloom` (imported as `bouquet`) |
| `.github/workflows` | CI and the Trusted-Publishing release workflows |

## Rust

The minimum supported Rust version is **1.95**. CI builds and tests on it, so
avoid newer language or library features.

Backends live behind cargo features (`classical`, `atomic`, `blocked`,
`simd`, plus `serde` and `std`). `serde` and `loom` are mutually exclusive,
so they are tested separately. Before opening a PR, run what CI runs:

```bash
cargo fmt --all --check
cargo clippy -p bouquet --all-targets -- -D warnings
cargo test -p bouquet                                       # default features
cargo test -p bouquet --features atomic,serde,blocked,simd  # everything but loom
cargo test -p bouquet --no-default-features --features classical   # no_std core
RUSTFLAGS="--cfg loom" LOOM_MAX_PREEMPTIONS=3 \
  cargo test -p bouquet --features loom,atomic --test loom_atomic --release
```

### `unsafe` code

The hot loops use `get_unchecked` and raw SIMD loads and stores. Their
soundness rests on invariants established at construction (the word count is
a whole number of blocks, and indices are bounded by the sizing math).

- Every `unsafe` block needs a `SAFETY:` comment naming the invariant it
  relies on, and every `unsafe fn` a `# Safety` section.
- Do not add a new construction path without enforcing those invariants, and
  add a test that goes through it.
- Changes near `unsafe` should pass Miri (nightly). CI does not run it on
  pull requests or on push (it is slow); a daily scheduled run covers `main`
  when the covered code changed (and every Monday regardless), and you can
  start it on demand via Actions → Miri → Run workflow. Run it yourself when
  you touch `unsafe`:
  ```bash
  MIRIFLAGS=-Zmiri-strict-provenance \
    cargo miri test -p bouquet --features atomic,serde,blocked --lib -- backend
  ```
  Filter on `backend`: the rest of the lib suite loops millions of times and
  takes hours under Miri. Add `--features simd` to cover the SIMD paths; on
  x86-64 also set `RUSTFLAGS="-C target-feature=+avx2"` to exercise AVX2.

### Tests worth knowing about

- `tests/hostile_input.rs` and `tests/limits.rs` pin the behaviour on
  malformed serialized input and on out-of-range sizes. A change that makes
  either panic or abort is a regression.
- The test suite also builds on 32-bit targets. Keep literals and shifts
  that need more than 32 bits behind `#[cfg(target_pointer_width = "64")]`.
- Changing a backend's probe derivation changes where bits land, which breaks
  persisted filters. If you must, change that backend's `Layout` tag so old
  blobs fail loudly instead of returning false negatives.

## Python bindings

```bash
cd bouquet-py
uv sync --locked                       # installs the pinned dev environment
uv run --no-sync maturin develop --release
uv run --no-sync pytest
uv run --no-sync ruff check . && uv run --no-sync ruff format --check .
uv run --no-sync ty check
```

Use `--locked` so you test what CI tests. `pyproject.toml` sets a one-week
dependency cooldown (`exclude-newer`), so a freshly published release cannot
enter the lockfile before it has aged.

If you change the API, update `bouquet.pyi` too. Bindings must raise Python
exceptions rather than panic or abort: validate inputs before they reach the
core crate's panicking constructors.

## Versioning

The crate and the Python package share one version, set once in the root
`Cargo.toml` under `[workspace.package]` and inherited by both with
`version.workspace = true`. A wheel is the core crate plus bindings built
from the same commit, so the version says which core it contains; a release
with no binding changes still gets a new Python version, matching the crate.

Releases are cut by tagging the same commit twice, `vX.Y.Z` (crates.io) and
`py-vX.Y.Z` (PyPI). Each release workflow refuses to publish if its tag
disagrees with the manifest (`.github/scripts/check-tag-version.sh`, tested
in CI). Published versions are immutable and tags are never moved.

## Pull requests

- Keep a PR to one logical change, and include tests for new behaviour or a
  fixed bug.
- Commit messages follow
  [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, `docs:`, `ci:`, `chore(deps):` ...), matching the
  existing history.
- Add a line to `CHANGELOG.md` for any user-visible change.
- Behaviour and performance claims in docs should be backed by a measurement
  or a test. If you change a number in the README, say how you got it.
- CI must be green. Workflow changes are linted with `actionlint` and
  `zizmor`; third-party actions are pinned to full commit SHAs, and
  Dependabot keeps the pins current.

## License

By contributing you agree that your contribution is licensed under the
repository's [MIT license](LICENSE).
