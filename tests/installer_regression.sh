#!/bin/sh
# Installs fake releases from a local file:// URL with install.sh and checks
# installation, replacement, checksum failure, and target validation.
set -eu

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
installer="$repo_root/install.sh"
root="$(mktemp -d "${TMPDIR:-/tmp}/sn-installer-test.XXXXXX")"
trap 'rm -rf "$root"' EXIT

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target="aarch64-apple-darwin" ;;
    Darwin-x86_64) target="x86_64-apple-darwin" ;;
    # musl targets skip the glibc probe, which would run the fake binary.
    Linux-aarch64 | Linux-arm64) target="aarch64-unknown-linux-musl" ;;
    Linux-x86_64) target="x86_64-unknown-linux-musl" ;;
    *)
        echo "skip: no release target for $(uname -s) $(uname -m)"
        exit 0
        ;;
esac

tag="v0.0.1"
archive="sn-$target.tar.gz"
release_directory="$root/releases/download/$tag"
install_directory="$root/install"
mkdir -p "$release_directory" "$install_directory"

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    else
        shasum -a 256 "$1" | awk '{ print $1 }'
    fi
}

write_archive() {
    payload="$root/payload"
    rm -rf "$payload"
    mkdir -p "$payload/sn-$target"
    printf '%s\n' "$1" > "$payload/sn-$target/sn"
    tar -czf "$release_directory/$archive" -C "$payload" "sn-$target"
    printf '%s  %s\n' "$(sha256 "$release_directory/$archive")" "$archive" > "$release_directory/$archive.sha256"
}

run_installer() {
    SN_VERSION=0.0.1 \
        SN_TARGET="${1:-$target}" \
        SN_INSTALL_DIR="$install_directory" \
        SN_RELEASES_URL="file://$root/releases" \
        sh "$installer" 2>/dev/null
}

installed() {
    cat "$install_directory/sn"
}

write_archive first
run_installer
[ "$(installed)" = first ] || { echo "install.sh did not install the payload" >&2; exit 1; }
echo "ok: install.sh installs a verified archive"

write_archive replacement
run_installer
[ "$(installed)" = replacement ] || { echo "install.sh did not replace the payload" >&2; exit 1; }
echo "ok: install.sh replaces an existing binary"

printf '%064d  %s\n' 0 "$archive" > "$release_directory/$archive.sha256"
if run_installer; then
    echo "a bad checksum unexpectedly succeeded" >&2
    exit 1
fi
[ "$(installed)" = replacement ] || { echo "a bad checksum replaced the binary" >&2; exit 1; }
echo "ok: a checksum failure keeps the existing binary"

if run_installer unsupported-target; then
    echo "an unsupported target unexpectedly succeeded" >&2
    exit 1
fi
echo "ok: an unsupported target fails"
