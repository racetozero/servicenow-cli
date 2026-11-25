default:
    @just --list

# Format all tracked files through prek.
fmt:
    @prek run --all-files

# Run every test.
test:
    @cargo nextest run --workspace

# Format check, lint with warnings as errors, and every test.
check: test
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Check that the other release platforms compile.
cross:
    cargo check -p sn --target x86_64-pc-windows-gnu
    cargo zigbuild -p sn --target x86_64-unknown-linux-musl

# Build the API catalog from a ServiceNowDocs checkout.
catalog DOCS:
    uv run scripts/build_catalog.py {{DOCS}}/markdown/api-reference/rest-apis

# Test the shell installer against a local release.
installer-test:
    sh tests/installer_regression.sh
    shellcheck -x install.sh tests/installer_regression.sh scripts/release.sh

# Build and verify the release archive for this machine.
release-check VERSION:
    scripts/release.sh check {{VERSION}}

# Install into ~/.cargo/bin.
install:
    cargo install --path crates/sn

# Test the PGO build and benchmark helpers.
pgo-test:
    @python3 scripts/test_pgo.py

# Build one native PGO release binary.
pgo-build target="":
    @python3 scripts/build_pgo.py --target-dir target/pgo {{ if target == "" { "" } else { "--target=" + target } }}

# Build matched ordinary and PGO binaries, then compare held-out workloads.
pgo-bench target="":
    @python3 scripts/build_pgo.py --baseline-and-pgo --target-dir target/pgo {{ if target == "" { "" } else { "--target=" + target } }}
    @python3 scripts/benchmark_pgo.py --target-dir target/pgo {{ if target == "" { "" } else { "--target=" + target } }} --json target/pgo/results.json
