# Releasing Bouquet

Bouquet has two independently versioned public packages:

| Package | Registry | Version source | Tag | Workflow |
| --- | --- | --- | --- | --- |
| `bouquet` | crates.io | `crates/bouquet/Cargo.toml` | `vX.Y.Z` | `release-crate.yml` |
| `bouquet-bloom` | PyPI | `bouquet-py/Cargo.toml` | `py-vX.Y.Z` | `release-wheels.yml` |

Both workflows use Trusted Publishing (OIDC), so GitHub stores no registry
token. The packages may have different versions, but when a change touches the
Rust API used by the Python bindings, release the Rust crate first and then the
Python package.

## One-time GitHub setup

In `klobucar/bouquet`, open **Settings → Environments → New environment** and
create `release`. Add yourself as a required reviewer. Allow deployments from
the `v*` and `py-v*` tag patterns, and protect those tag patterns with repository
tag rules.

The environment is included in the OIDC identity. Its spelling must match the
publisher configuration on both registries.

## One-time crates.io setup

crates.io cannot create a trusted publisher before the crate exists. The first
`bouquet` release must therefore be published manually.

1. Merge the release workflows to `main`. In GitHub, open **Actions → Release
   crate → … → Disable workflow**. This prevents the initial tag push from
   attempting OIDC before crates.io can know that publisher.
2. Sign in to crates.io with GitHub, verify your email, and create a short-lived
   API token suitable for publishing a new crate.
3. From the clean release commit, verify exactly what will ship:

   ```bash
   cargo fmt --all --check
   cargo test -p bouquet --locked
   cargo test -p bouquet --locked --features atomic,serde,blocked,simd
   cargo test -p bouquet --locked --features loom,atomic
   cargo test -p bouquet --locked --no-default-features --features classical
   cargo package -p bouquet --locked
   cargo package -p bouquet --locked --list
   ```

   `serde` and `loom` are mutually exclusive, which is why they are tested in
   separate commands. Confirm `LICENSE` appears in the package list.

4. Authenticate and publish the initial version:

   ```bash
   cargo login
   cargo publish -p bouquet --locked
   cargo logout
   ```

5. Tag that exact commit while the workflow is still disabled:

   ```bash
   git tag -a v0.1.0 -m "bouquet 0.1.0"
   git push origin v0.1.0
   ```

6. Open the `bouquet` crate on crates.io, choose **Settings → Trusted
   Publishing**, and add:

   | Field | Value |
   | --- | --- |
   | Repository owner | `klobucar` |
   | Repository name | `bouquet` |
   | Workflow file | `release-crate.yml` |
   | Environment | `release` |

7. Revoke the bootstrap API token, then re-enable **Release crate** in GitHub
   Actions. Future versions use OIDC only. Create the initial GitHub Release
   from `v0.1.0`; do not rerun the publish workflow for that already-uploaded
   version.

## One-time PyPI setup

PyPI supports pending publishers, so `bouquet-bloom` does not need a manual
bootstrap upload. Sign in to PyPI and open **Your account → Publishing → Add a
new pending publisher**:

| Field | Value |
| --- | --- |
| PyPI project name | `bouquet-bloom` |
| Owner | `klobucar` |
| Repository | `bouquet` |
| Workflow name | `release-wheels.yml` |
| Environment | `release` |

If the project already exists, add the same identity under **Manage project →
Publishing**. A pending publisher does not reserve the project name; publish the
first release promptly after configuring it.

## Rust crate release

1. Start from a clean `main` with CI passing.
2. Update `version` in `crates/bouquet/Cargo.toml`, update `CHANGELOG.md`, and
   regenerate `Cargo.lock`.
3. Run the CI-equivalent checks and package rehearsal:

   ```bash
   cargo fmt --all --check
   cargo test -p bouquet --locked
   cargo package -p bouquet --locked
   cargo package -p bouquet --locked --list
   ```

4. Commit and merge the release changes.
5. Run **Actions → Release crate → Run workflow** with `dry_run` enabled.
6. Tag the exact verified commit and push it:

   ```bash
   git switch main
   git pull --ff-only
   git tag -a v0.1.1 -m "bouquet 0.1.1"
   git push origin v0.1.1
   ```

7. Approve the `release` environment deployment. Confirm the version on
   crates.io and that docs.rs built successfully.
8. Create a GitHub Release for `v0.1.1` using the matching changelog section.

## Python wheel release

1. Update `version` in `bouquet-py/Cargo.toml` and the Python-facing changelog.
   The Python version comes from Cargo metadata; `pyproject.toml` is deliberately
   dynamic.
2. Regenerate the workspace lockfile and the Python development lockfile:

   ```bash
   cargo update -p bouquet-py
   uv lock --project bouquet-py
   ```

3. Verify the bindings locally:

   ```bash
   cargo test -p bouquet-py --locked
   cd bouquet-py
   uv sync --locked
   uv run maturin develop --release --locked
   uv run pytest tests
   uv run maturin sdist --out dist
   cd ..
   ```

4. Commit and merge the release changes.
5. Run **Actions → Release wheels → Run workflow** with `dry_run` enabled and
   inspect the wheel artifacts for Linux x86-64/aarch64, macOS Intel/Apple
   Silicon, and Windows x64, plus the source distribution.
6. Tag the exact verified commit and push it:

   ```bash
   git switch main
   git pull --ff-only
   git tag -a py-v0.1.0 -m "bouquet-bloom 0.1.0"
   git push origin py-v0.1.0
   ```

7. Approve the `release` deployment and verify installation from PyPI:

   ```bash
   uv run --isolated --with bouquet-bloom==0.1.0 \
     python -c "import bouquet; print(bouquet.__version__)"
   ```

8. Create a GitHub Release for the `py-v0.1.0` tag.

## Failure and recovery

Registry versions are immutable. Never move a published tag or attempt to reuse
its version. If a release is broken, yank it on crates.io or PyPI, fix the issue,
and publish a new patch version. Yanking prevents new dependency resolution but
does not delete the artifact or break existing lockfiles.

For OIDC failures, compare the owner, repository, workflow filename, and
environment against the workflow. `id-token: write` belongs only on the publish
job. Repository or workflow renames require updating both trusted publishers.
