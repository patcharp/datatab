#!/bin/sh
# Build the stable Debian package from an already built release binary.
# Usage: scripts/package-deb.sh <binary> <version> <output.deb>
# Never runs cargo. Uses nfpm from PATH, else the pinned nfpm container.
set -eu

NFPM_IMAGE="goreleaser/nfpm:v2.49.0"
TARGET_TRIPLE="x86_64-unknown-linux-gnu"
PACKAGE_NAME="datatab"
APP_NAME="DataTab"
APP_ID="datatab"
ARCH="amd64"
BRAND_DIR="resources/branding/stable"
# Highest library versions the package may need. They must match the depends
# in packaging/nfpm.deb.yaml: libc6 (>= 2.39) and libstdc++6 (>= 13).
MAX_GLIBC="2.39"
MAX_GLIBCXX="3.4.32"

fail() {
    echo "package-deb: $*" >&2
    exit 1
}

if [ "$#" -ne 3 ]; then
    echo "usage: $0 <binary> <version> <output.deb>" >&2
    exit 2
fi
binary=$1
version=$2
output=$3

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

case $version in
    '' | *[!0-9A-Za-z.+~-]*) fail "version '$version' may only contain letters, digits and . + ~ -" ;;
esac
[ -f "$binary" ] || fail "binary not found: $binary"
[ -r "$binary" ] || fail "binary is not readable: $binary"
case $output in
    *.deb) ;;
    *) fail "output path must end in .deb: $output" ;;
esac

# Print the highest version of one symbol-version family (GLIBC, GLIBCXX) found
# in the readelf output given as $1, or nothing when it has none.
highest_needed() {
    printf '%s\n' "$1" | grep -o "@$2_[0-9][0-9.]*" | sed "s/^@$2_//" |
        sort -u -V | tail -n 1
}

# Refuse a binary that needs a newer library than the declared depends.
check_library_floor() {
    symbols=$(readelf --dyn-syms -W "$1" 2>&1) ||
        fail "readelf could not read the dynamic symbols of $1: $symbols"
    for family in GLIBC GLIBCXX; do
        if [ "$family" = GLIBC ]; then limit=$MAX_GLIBC; else limit=$MAX_GLIBCXX; fi
        needed=$(highest_needed "$symbols" "$family")
        if [ -z "$needed" ]; then
            [ "$family" = GLIBCXX ] || fail "found no GLIBC symbol versions in $1; is it a glibc release build?"
            echo "package-deb: found no GLIBCXX symbol versions in $1; skipping the libstdc++ check" >&2
            continue
        fi
        newest=$(printf '%s\n%s\n' "$needed" "$limit" | sort -V | tail -n 1)
        [ "$newest" = "$limit" ] ||
            fail "the binary needs ${family}_$needed but the package depends allow only ${family}_$limit; build the binary on Ubuntu 24.04"
    done
}

for required in \
    packaging/nfpm.deb.yaml \
    packaging/datatab.desktop \
    packaging/icons \
    packaging/scripts/postinst \
    "$BRAND_DIR/mark.svg" \
    LICENSE-MIT \
    LICENSE-APACHE; do
    [ -e "$repo_root/$required" ] || fail "missing input in the repository: $required"
done

for tool in envsubst sed grep cp chmod mkdir mktemp dirname basename readelf sort tail; do
    command -v "$tool" >/dev/null 2>&1 || fail "required tool not found: $tool"
done
if command -v nfpm >/dev/null 2>&1; then
    runner=nfpm
elif command -v docker >/dev/null 2>&1; then
    runner=docker
else
    fail "neither nfpm nor docker is on PATH; install one of them"
fi

check_library_floor "$binary"

output_dir=$(dirname -- "$output")
mkdir -p -- "$output_dir"
output_dir=$(CDPATH= cd -- "$output_dir" && pwd)
output_name=$(basename -- "$output")
binary_dir=$(CDPATH= cd -- "$(dirname -- "$binary")" && pwd)
binary="$binary_dir/$(basename -- "$binary")"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
trap 'exit 1' HUP INT TERM

mkdir -p "$stage/target/$TARGET_TRIPLE/release" "$stage/resources/branding"
cp -- "$binary" "$stage/target/$TARGET_TRIPLE/release/datatab"
chmod 755 "$stage/target/$TARGET_TRIPLE/release/datatab"
cp -R "$repo_root/packaging" "$stage/packaging"
cp -R "$repo_root/$BRAND_DIR" "$stage/$BRAND_DIR"
cp -- "$repo_root/LICENSE-MIT" "$repo_root/LICENSE-APACHE" "$stage/"

sed -e "s/@APP_NAME@/$APP_NAME/g" -e "s/@APP_ID@/$APP_ID/g" \
    "$repo_root/packaging/datatab.desktop" >"$stage/datatab.desktop"
if grep -q '@' "$stage/datatab.desktop"; then
    fail "the rendered desktop file still has an unfilled placeholder"
fi

# nfpm does not expand ${...} in contents, so render the config first. The
# explicit list keeps envsubst from touching any other $ in the file.
# shellcheck disable=SC2016
PKG_NAME=$PACKAGE_NAME APP_ID=$APP_ID ARCH=$ARCH VERSION=$version \
    TARGET=$TARGET_TRIPLE BRAND_DIR=$BRAND_DIR \
    envsubst '${PKG_NAME} ${APP_ID} ${ARCH} ${VERSION} ${TARGET} ${BRAND_DIR}' \
    <"$repo_root/packaging/nfpm.deb.yaml" >"$stage/nfpm.rendered.yaml"
grep -q '^  - src: packaging/datatab.desktop$' "$stage/nfpm.rendered.yaml" ||
    fail "packaging/nfpm.deb.yaml has no desktop entry to point at the rendered file"
sed 's#^  - src: packaging/datatab.desktop$#  - src: datatab.desktop#' \
    "$stage/nfpm.rendered.yaml" >"$stage/nfpm.pointed.yaml"
mv "$stage/nfpm.pointed.yaml" "$stage/nfpm.rendered.yaml"

if [ "$runner" = nfpm ]; then
    (cd "$stage" && nfpm package --config nfpm.rendered.yaml --packager deb \
        --target "$output_dir/$output_name")
else
    docker run --rm --memory 512m --cpus 1 -u "$(id -u):$(id -g)" \
        -v "$stage:/src" -v "$output_dir:/out" -w /src \
        "$NFPM_IMAGE" package --config nfpm.rendered.yaml --packager deb \
        --target "/out/$output_name"
fi

[ -f "$output_dir/$output_name" ] || fail "nfpm finished but $output_dir/$output_name was not written"
echo "package-deb: wrote $output_dir/$output_name"
