#!/bin/sh
# Build DataTab on Linux, and optionally the .deb package.
#
# Run from the root of a DataTab checkout:
#   sh scripts/build-linux.sh           # release binary only
#   sh scripts/build-linux.sh --deb     # release binary and the .deb
#   JOBS=2 sh scripts/build-linux.sh    # fewer build jobs (default 4)
#
# Needs: rustup (cargo and rustc), mold, cmake, pkg-config and the development
# libraries listed in docs/BUILDING.md. For --deb it also needs readelf,
# envsubst and either nfpm or Docker; build on x86_64 Ubuntu 24.04.
# The package is amd64 only.
# Set CARGO_TARGET_DIR to build outside ./target.
# Writes <target>/linux-package/ and leaves the tracked tree clean.
set -eu

APT_LINE="sudo apt-get install -y mold cmake pkg-config libssl-dev libdbus-1-dev libxkbcommon-dev libxkbcommon-x11-dev libpipewire-0.3-dev libgbm-dev libegl-dev libxcb-randr0-dev libclang-dev libfontconfig1-dev libvulkan-dev"

fail() {
    echo "build-linux: $*" >&2
    exit 1
}

usage() {
    echo "usage: sh scripts/build-linux.sh [--deb]   (JOBS=N sets the build jobs, default 4)"
}

make_deb=no
for argument in "$@"; do
    case $argument in
        --deb) make_deb=yes ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

[ "$(uname -s)" = Linux ] || fail "run this on Linux"
for required in Cargo.toml scripts/package-deb.sh LICENSE-MIT LICENSE-APACHE; do
    [ -e "$required" ] || fail "missing $required; is this a DataTab checkout?"
done

missing=""
for tool in cargo rustc mold cmake pkg-config sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    echo "build-linux: required tools not found:$missing" >&2
    echo "build-linux: on Ubuntu or Debian, install the build dependencies with:" >&2
    echo "  $APT_LINE" >&2
    echo "build-linux: install Rust with rustup from https://rustup.rs when cargo or rustc is missing" >&2
    exit 1
fi
if [ "$make_deb" = yes ]; then
    command -v readelf >/dev/null 2>&1 ||
        fail "readelf not found; install it with: sudo apt-get install -y binutils"
    command -v envsubst >/dev/null 2>&1 ||
        fail "envsubst not found; install it with: sudo apt-get install -y gettext-base"
    if ! command -v nfpm >/dev/null 2>&1 && ! command -v docker >/dev/null 2>&1; then
        fail "--deb needs nfpm or Docker on PATH; install one of them"
    fi
fi

jobs=${JOBS:-4}
case $jobs in
    '' | *[!0-9]* | 0) fail "JOBS must be a positive whole number, got '$jobs'" ;;
esac

version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)
[ -n "$version" ] || fail "could not read the version from [workspace.package] in Cargo.toml"

commit=unknown
dirty=""
if command -v git >/dev/null 2>&1 && git rev-parse HEAD >/dev/null 2>&1; then
    commit=$(git rev-parse HEAD)
    [ -z "$(git status --porcelain)" ] || dirty=" (with local changes)"
fi

target_dir=${CARGO_TARGET_DIR:-target}
binary="$target_dir/release/datatab"
stage="$target_dir/linux-package"

# 1. Build the release binary.
set +e
CARGO_BUILD_JOBS=$jobs cargo build -p datatab --release --locked
build_exit=$?
set -e
[ "$build_exit" -eq 0 ] || fail "cargo build failed with exit code $build_exit; if it ran out of memory, try JOBS=2 or JOBS=1"
[ -f "$binary" ] || fail "cargo finished but $binary is missing"

mkdir -p "$stage"
deb="$stage/datatab-linux-amd64.deb"
report="$stage/report.txt"
rm -f "$deb" "$report"

# 2. Build the package.
deb_exit="not run"
if [ "$make_deb" = yes ]; then
    elf_header=$(readelf -h "$binary" 2>&1) || fail "readelf could not read $binary: $elf_header"
    case $elf_header in
        *"Advanced Micro Devices X86-64"*) ;;
        *) fail "$binary is not an x86_64 binary; the package is amd64 only, so build on an x86_64 Linux machine" ;;
    esac
    set +e
    sh scripts/package-deb.sh "$binary" "$version" "$deb"
    deb_exit=$?
    set -e
    [ "$deb_exit" -eq 0 ] || fail "package-deb.sh failed with exit code $deb_exit"
    [ -f "$deb" ] || fail "package-deb.sh finished but $deb is missing"
fi

# 3. Report.
hash_output=$(sha256sum "$binary") || fail "sha256sum failed on $binary"
binary_hash=${hash_output%% *}
[ -n "$binary_hash" ] || fail "sha256sum printed no hash for $binary"
deb_hash=""
if [ "$make_deb" = yes ]; then
    hash_output=$(sha256sum "$deb") || fail "sha256sum failed on $deb"
    deb_hash=${hash_output%% *}
    [ -n "$deb_hash" ] || fail "sha256sum printed no hash for $deb"
fi
{
    echo "Linux build report"
    echo "- commit: $commit$dirty"
    echo "- $(rustc --version)"
    echo "- version: $version"
    echo "- cargo build -p datatab --release --locked: exit $build_exit, jobs $jobs"
    echo "- $binary SHA-256: $binary_hash"
    if [ "$make_deb" = yes ]; then
        echo "- scripts/package-deb.sh: exit $deb_exit"
        echo "- $deb SHA-256: $deb_hash"
    fi
} >"$report"
echo
cat "$report"
echo "build-linux: report saved to $report"
