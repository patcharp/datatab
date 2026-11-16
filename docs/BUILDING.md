# Building DataTab from source

Each platform builds on its own operating system. Windows cannot be cross-compiled from Linux, because the GPUI Windows crate compiles its shaders with `fxc.exe` only on a Windows host and `crates/datatab/build.rs` embeds the icon and version resources only on a Windows host.

DataTab supports Windows, Linux and macOS, on Intel and on Apple Silicon. macOS has a local build script, described at the end.

Every script below:

- is run from the root of the checkout;
- builds with `cargo build -p datatab --release --locked`;
- uses 4 build jobs by default. A full release build peaked at about 8 GB of RAM with 2 jobs on Linux, so lower the jobs when the build runs out of memory;
- builds under `target`, or under `CARGO_TARGET_DIR` when you set it;
- writes its output under `<target>/<platform>-package/`, so the tracked tree stays clean;
- prints a short report and saves it as `report.txt` next to the output.

[Rust with rustup](https://rustup.rs) is needed on every platform. `rust-toolchain.toml` selects the version.

## Linux

Prerequisites on Ubuntu 24.04 or Debian 13:

```bash
sudo apt-get install -y mold cmake pkg-config libssl-dev libdbus-1-dev libxkbcommon-dev libxkbcommon-x11-dev libpipewire-0.3-dev libgbm-dev libegl-dev libxcb-randr0-dev libclang-dev libfontconfig1-dev libvulkan-dev
```

Other distributions need the same libraries under their own package names. `mold` is required: `.cargo/config.toml` links every Linux build with it. For the `.deb` you also need `readelf` (`sudo apt-get install -y binutils`), `envsubst` (`sudo apt-get install -y gettext-base`) and Docker for the pinned nfpm container used by default. Install Docker with `sudo apt-get install -y docker.io` and add yourself to the `docker` group. To use a local `nfpm` instead, install it from [nfpm.goreleaser.com/install](https://nfpm.goreleaser.com/install/) and set `DATATAB_NFPM_LOCAL=1`. The package is amd64 only, so `--deb` fails on a binary that is not x86_64.

Run one of these:

```bash
sh scripts/build-linux.sh          # the release binary
sh scripts/build-linux.sh --deb    # the binary and the .deb package
JOBS=2 sh scripts/build-linux.sh   # fewer build jobs
```

It writes:

- `target/release/datatab`, the binary;
- `target/linux-package/datatab-linux-amd64.deb`, with `--deb`;
- `target/linux-package/report.txt`: commit, `rustc --version`, exit codes and SHA-256 of the binary and the package.

Run the binary with `target/release/datatab`. Install the package with:

```bash
sudo apt install ./target/linux-package/datatab-linux-amd64.deb
```

The package depends on `libc6 (>= 2.39)` and `libstdc++6 (>= 13)`, so build it on Ubuntu 24.04 or Debian 13 or later. `scripts/package-deb.sh` refuses a binary that needs newer libraries. The package is unsigned.

### Linux failures

- **A tool is missing.** The script names the missing tools and prints the `apt-get` line above. Install them and run the script again. Install Rust with [rustup](https://rustup.rs) when `cargo` is missing.
- **The build runs out of memory.** A full release build peaked at about 8 GB of RAM with 2 jobs. Run `JOBS=1 sh scripts/build-linux.sh`. It is slower and needs less memory. Closing other programs also helps.
- **`--deb` cannot find its packaging tools.** Docker is required by default for the pinned nfpm container. A local `nfpm` is used only with `DATATAB_NFPM_LOCAL=1`. Install the selected tool as shown above, and `sudo apt-get install -y gettext-base binutils` for `envsubst` and `readelf`.
- **`--deb` says the binary is not an x86_64 binary.** The package is amd64 only. Build on an x86_64 machine.
- **The package script says the binary needs a newer `GLIBC`.** You built on a system newer than Ubuntu 24.04. Build on Ubuntu 24.04.

## Windows

Prerequisites: Windows 10 or later on x86_64, and these tools. The `winget` lines are one way to install them:

```powershell
winget install Git.Git
winget install Rustlang.Rustup
winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install JRSoftware.InnoSetup
```

The C++ workload of the [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) brings the Windows SDK and `fxc.exe`. [Inno Setup 6](https://jrsoftware.org/isinfo.php) builds the installer; it is not needed with `-SkipInstaller`. Open a new terminal after installing, so `cargo` is on `PATH`.

Run one of these in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1
powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -SkipInstaller
powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -Jobs 2
```

`-SkipInstaller` builds only `datatab.exe`. The script runs on Windows PowerShell 5.1 and on PowerShell 7.

It writes:

- `target\release\datatab.exe`, the executable;
- `target\windows-package\output\datatab-windows-amd64-setup.exe`, the installer, unless you pass `-SkipInstaller`;
- `target\windows-package\report.txt`: commit, OS, `rustc --version`, exit codes, the executable's version resources and SHA-256 of the executable and the installer.

Run `datatab.exe` directly, or run the installer and follow its prompts. The installer is unsigned, so Windows may show a publisher warning.

### Windows failures

- **"Running scripts is disabled on this system".** The PowerShell execution policy blocks the script. Start it as shown above, with `powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1`. This changes the policy for that one run only.
- **A tool is missing.** The script names it. Install it with the `winget` lines above, then open a new terminal so `cargo` is on `PATH`.
- **`ISCC.exe` is not found.** Install Inno Setup 6, or pass `-SkipInstaller`.
- **The build fails on `fxc.exe` or with a linker error.** The Build Tools lack the C++ workload or the Windows SDK. Open the Visual Studio Installer, choose Modify, and add "Desktop development with C++".
- **The build runs out of memory.** Run the script with `-Jobs 1`. It is slower and needs less memory.

## Run a debug build

On any platform, from a checkout:

```bash
cargo run -p datatab
```

## Build with other features

The scripts build the default feature set. To build with other features, call cargo yourself:

```bash
# Full default feature set
cargo build -p datatab --release

# The default features with MCP turned off, and Lua kept
cargo build -p datatab --release --no-default-features --features sqlite,postgres,mysql,mongodb,redis,influxdb,mssql,redshift,clickhouse,turso,duckdb,lua

# Minimal relational drivers, without AI/MCP or Lua
cargo build -p datatab --release --no-default-features --features sqlite,postgres,mysql
```

The binary is `target/release/datatab` on Linux and `target\release\datatab.exe` on Windows.

## macOS

macOS is supported on Intel and on Apple Silicon, and the release workflow builds a `DataTab.app` for each. `scripts/build-macos.sh` is the local macOS build. Local builds use an ad-hoc signature by default. macOS releases use the self-signed PCPLab project certificate and are not notarized. Set `MACOS_SIGN_IDENTITY` to use an installed project signing identity. See [Installing DataTab](INSTALL.md#macos).

Prerequisites: rustup, `cmake` (`brew install cmake`), and the full Xcode app from the App Store. The Command Line Tools are not enough: GPUI compiles its Metal shaders with `xcrun metal`. After installing Xcode:

```bash
sudo xcode-select -s /Applications/Xcode.app
xcodebuild -downloadComponent MetalToolchain   # Xcode 26 or later
```

Run:

```bash
sh scripts/build-macos.sh
JOBS=2 sh scripts/build-macos.sh   # fewer build jobs
```

Without an argument, the script builds for the Mac it runs on. To build for the other architecture, add its standard library once and pass its target triple. The release workflow builds the Intel app this way on an Apple Silicon runner:

```bash
rustup target add x86_64-apple-darwin
sh scripts/build-macos.sh x86_64-apple-darwin    # Intel app
sh scripts/build-macos.sh aarch64-apple-darwin   # Apple Silicon app
```

The script builds OpenSSL from source (the `vendored-openssl` feature), so the app does not depend on a Homebrew OpenSSL. That step uses `perl` and `make`, which macOS and Xcode provide. The script checks the binary's architecture with `lipo -archs` before it builds the bundle.

It writes:

- `target/macos-package/DataTab.app`, with the icon, `Info.plist` and an ad-hoc signature by default, or the project signature when `MACOS_SIGN_IDENTITY` is set;
- `target/macos-package/report.txt`: commit, macOS version, `rustc --version`, the cargo command and its exit code, the binary's architecture and its SHA-256.

To also make the disk image the release ships, run `sh scripts/build-macos.sh --dmg-only arm64` (or `x86_64`) after the build. It writes `dist/datatab-macos-<arch>.dmg`, with `DataTab.app` and an `Applications` link, and replaces an existing image of the same name.

Open it with `open target/macos-package/DataTab.app`, or drag it to `/Applications`. For a bundle you copied from another Mac, clear the quarantine flag first:

```bash
xattr -dr com.apple.quarantine DataTab.app
```

### macOS failures

- **"The Metal compiler is missing".** Xcode is not selected or lacks the Metal toolchain. Run the two commands above.
- **The build fails in `aws-lc-sys`.** `cmake` is missing. Run `brew install cmake`.
- **The build runs out of memory.** Run `JOBS=1 sh scripts/build-macos.sh`. If the final link of `datatab` alone runs out of memory, turn thin LTO off for that build: `CARGO_PROFILE_RELEASE_LTO=false sh scripts/build-macos.sh`.
- **"the Rust standard library for x86_64-apple-darwin is not installed".** Run `rustup target add x86_64-apple-darwin` in the checkout, then build again.
- **macOS says the app is damaged or from an unidentified developer.** The bundle is not notarized. Clear the quarantine flag as shown above, or open it from its context menu.
