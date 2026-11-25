# Releasing

Pushing a `v*` tag starts `.github/workflows/release.yml`, which uses [cargo-dist](https://github.com/axodotdev/cargo-dist) 0.32. It builds archives for eight targets with checksums and attestations, creates the GitHub release, and commits the Homebrew formula to `racetozero/homebrew-tap`. The formula uses the static musl builds on Linux.

Every target except Windows on ARM is built with profile-guided optimization (PGO): the build job trains an instrumented binary on the workloads in `scripts/pgo_common.py`, then builds the release with that profile. `just pgo-bench` builds an ordinary and a PGO binary here and compares them on held-out workloads.

`install.sh`, `install.ps1` and `sn update` download these archives and verify their SHA-256 checksums.

The agent skill in `.agents/skills` is served from `main`; `npx skills add racetozero/servicenow-cli` reads it there.

## One-time setup

- The repository `racetozero/servicenow-cli`.
- The secret `HOMEBREW_TAP_DEPLOY_KEY`: a deploy key with write access to `racetozero/homebrew-tap`.

## Each release

1. Set the version in `Cargo.toml` (`[workspace.package]` and the `sn-*` workspace dependencies). In `CHANGELOG.md`, add a `## <version>` section.
2. Run `just release-check <version>`. It builds the archive for this machine and checks it.
3. Run `scripts/release.sh release <version>`. It runs the checks again, then pushes `main` and the tag.

After a dist upgrade, `dist generate` removes the overrides listed in the header of `release.yml`; reapply them.

## Updating the API catalog

```sh
git clone --depth 1 --branch brazil https://github.com/ServiceNow/ServiceNowDocs
just catalog ServiceNowDocs
```

The script stops when it finds an endpoint that it cannot read.
