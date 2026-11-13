#!/bin/sh
# Build DataTab.app on a Mac. The bundle is unsigned (ad-hoc signature only).
#
# Run from the root of a DataTab checkout:
#   sh scripts/build-macos.sh                       # for this Mac, 4 build jobs
#   JOBS=8 sh scripts/build-macos.sh
#   sh scripts/build-macos.sh x86_64-apple-darwin   # Intel app, on any Mac
#   sh scripts/build-macos.sh aarch64-apple-darwin  # Apple Silicon app, on any Mac
#
# The optional argument is the target triple. Without it, cargo builds for this
# Mac. Building for the other architecture needs its standard library first:
#   rustup target add x86_64-apple-darwin
# OpenSSL is always built from source (the vendored-openssl feature), so the app
# never depends on a Homebrew OpenSSL and the cross build needs none for the
# other architecture.
#
# Needs: rustup, full Xcode (not only the Command Line Tools: GPUI compiles its
# Metal shaders with `xcrun metal`), and cmake (brew install cmake).
# Set CARGO_TARGET_DIR to build outside ./target.
# Writes <target>/macos-package/DataTab.app and leaves the tracked tree clean.
set -eu

fail() {
    echo "build-macos: $*" >&2
    exit 1
}

[ "$(uname -s)" = Darwin ] || fail "run this on macOS"
[ "$#" -le 1 ] || fail "usage: sh scripts/build-macos.sh [x86_64-apple-darwin|aarch64-apple-darwin]"
target=${1:-}
case $target in
    '') expected_arch=$(uname -m) ;;
    x86_64-apple-darwin) expected_arch=x86_64 ;;
    aarch64-apple-darwin) expected_arch=arm64 ;;
    *) fail "unsupported target '$target'; use x86_64-apple-darwin or aarch64-apple-darwin" ;;
esac
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"
for required in Cargo.toml packaging/icons/512x512/apps/datatab.png LICENSE-MIT LICENSE-APACHE; do
    [ -e "$required" ] || fail "missing $required; is this a DataTab checkout?"
done
for tool in git cargo rustc xcrun iconutil sips plutil codesign shasum lipo; do
    command -v "$tool" >/dev/null 2>&1 || fail "required tool not found: $tool; install rustup from https://rustup.rs and Xcode from the App Store"
done
xcrun -sdk macosx -f metal >/dev/null 2>&1 ||
    fail "the Metal compiler is missing. Install full Xcode, run 'sudo xcode-select -s /Applications/Xcode.app', and on Xcode 26 or later also 'xcodebuild -downloadComponent MetalToolchain'"
command -v cmake >/dev/null 2>&1 ||
    echo "build-macos: note: cmake not found; if the build fails in aws-lc-sys, run 'brew install cmake'" >&2
if [ -n "$target" ]; then
    sysroot=$(rustc --print sysroot) || fail "rustc --print sysroot failed"
    [ -d "$sysroot/lib/rustlib/$target/lib" ] ||
        fail "the Rust standard library for $target is not installed; run 'rustup target add $target'"
fi

commit=$(git rev-parse HEAD)
dirty=""
[ -z "$(git status --porcelain)" ] || dirty=" (with local changes)"
version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)
[ -n "$version" ] || fail "could not read the version from [workspace.package] in Cargo.toml"
# The bundle version keys must be numeric X.Y.Z, so drop any -rc.N or +build suffix.
bundle_version=${version%%[-+]*}
case $bundle_version in
    '' | *[!0-9.]*) fail "cannot derive a numeric bundle version from '$version'" ;;
esac

# 1. Build the release binary.
jobs=${JOBS:-4}
case $jobs in
    '' | *[!0-9]* | 0) fail "JOBS must be a positive whole number, got '$jobs'" ;;
esac
target_dir=${CARGO_TARGET_DIR:-target}
build_command="cargo build -p datatab --release --locked --features vendored-openssl"
if [ -n "$target" ]; then
    build_command="$build_command --target $target"
    binary="$target_dir/$target/release/datatab"
else
    binary="$target_dir/release/datatab"
fi
start=$(date +%s)
set +e
# shellcheck disable=SC2086 # the command is built from fixed words only
CARGO_BUILD_JOBS=$jobs $build_command
build_exit=$?
set -e
minutes=$(( ($(date +%s) - start) / 60 ))
[ "$build_exit" -eq 0 ] || fail "cargo build failed with exit code $build_exit; if it ran out of memory, try JOBS=2 or JOBS=1"
[ -f "$binary" ] || fail "cargo finished but $binary is missing"
binary_arch=$(lipo -archs "$binary") || fail "lipo could not read $binary"
[ "$binary_arch" = "$expected_arch" ] ||
    fail "$binary is built for '$binary_arch', expected '$expected_arch'"

# 2. Assemble the bundle.
stage="$target_dir/macos-package"
app="$stage/DataTab.app"
rm -rf "$stage"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$stage/datatab.iconset"
cp "$binary" "$app/Contents/MacOS/datatab"
chmod 755 "$app/Contents/MacOS/datatab"
cp LICENSE-MIT LICENSE-APACHE "$app/Contents/Resources/"

icons=packaging/icons
iconset="$stage/datatab.iconset"
cp "$icons/16x16/apps/datatab.png" "$iconset/icon_16x16.png"
cp "$icons/32x32/apps/datatab.png" "$iconset/icon_16x16@2x.png"
cp "$icons/32x32/apps/datatab.png" "$iconset/icon_32x32.png"
cp "$icons/64x64/apps/datatab.png" "$iconset/icon_32x32@2x.png"
cp "$icons/128x128/apps/datatab.png" "$iconset/icon_128x128.png"
cp "$icons/256x256/apps/datatab.png" "$iconset/icon_128x128@2x.png"
cp "$icons/256x256/apps/datatab.png" "$iconset/icon_256x256.png"
cp "$icons/512x512/apps/datatab.png" "$iconset/icon_256x256@2x.png"
cp "$icons/512x512/apps/datatab.png" "$iconset/icon_512x512.png"
sips -z 1024 1024 "$icons/512x512/apps/datatab.png" --out "$iconset/icon_512x512@2x.png" >/dev/null
iconutil -c icns "$iconset" -o "$app/Contents/Resources/datatab.icns"
rm -rf "$iconset"

cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>DataTab</string>
    <key>CFBundleDisplayName</key><string>DataTab</string>
    <key>CFBundleIdentifier</key><string>me.pcplab.datatab</string>
    <key>CFBundleExecutable</key><string>datatab</string>
    <key>CFBundleIconFile</key><string>datatab</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$bundle_version</string>
    <key>CFBundleVersion</key><string>$bundle_version</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>PCPLab. MIT or Apache-2.0.</string>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist" >/dev/null || fail "Info.plist is not valid"

# Ad-hoc signature: Apple Silicon refuses to run an unsigned binary, and the
# bundle changed after the linker signed it.
codesign --force --deep --sign - "$app"
codesign --verify --deep "$app" || fail "the ad-hoc signature does not verify"

# 3. Report.
hash_output=$(shasum -a 256 "$app/Contents/MacOS/datatab") || fail "shasum failed on the bundle binary"
binary_hash=${hash_output%% *}
[ -n "$binary_hash" ] || fail "shasum printed no hash for the bundle binary"
report="$stage/report.txt"
cat >"$report" <<EOF
macOS build report
- commit: $commit$dirty
- macOS $(sw_vers -productVersion) on $(uname -m)
- $(rustc --version)
- version: $version (bundle version $bundle_version)
- $build_command: exit $build_exit, about $minutes min, jobs $jobs
- release profile overrides: lto ${CARGO_PROFILE_RELEASE_LTO:-from Cargo.toml}, codegen-units ${CARGO_PROFILE_RELEASE_CODEGEN_UNITS:-from Cargo.toml}
- binary architecture: $binary_arch
- DataTab.app/Contents/MacOS/datatab SHA-256: $binary_hash
- bundle: $(cd "$stage" && pwd)/DataTab.app
EOF
echo
cat "$report"
echo "build-macos: open it with: open $app   (or drag it to /Applications)"
