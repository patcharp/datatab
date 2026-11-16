# Installing DataTab

DataTab supports Windows, Linux and macOS, on Intel and on Apple Silicon. Download the installer from the releases of the public repository, `https://github.com/patcharp/datatab/releases`.

## Windows

Run the Windows installer and follow its prompts. The installer is unsigned, so Windows may show a publisher warning.

### Requirements

Windows 10 or later on x86_64. Install the Visual Studio Build Tools with the C++ workload if building from source.

## Debian and Ubuntu

Install the `.deb` package you received:

```bash
sudo apt install ./datatab-linux-amd64.deb
```

Use the file name of the package you received. The package is unsigned.

## macOS

The release has a `DataTab.app` for Intel and one for Apple Silicon. Both need macOS 11 or later. Open the `.dmg` for your architecture and drag DataTab to Applications. The `.zip` also works: unzip it and drag DataTab to Applications.

From 0.1.3 onward, releases carry a stable signature from the self-signed PCPLab project certificate. They are not notarized, so Gatekeeper still warns on first start. Clear the downloaded app's quarantine flag, then open it:

```bash
xattr -dr com.apple.quarantine /Applications/DataTab.app
open /Applications/DataTab.app
```

Keychain may ask for access to saved passwords after an upgrade. Choose **Always Allow** if you want to grant access. Whether macOS keeps that grant across updates is not verified yet: Apple's securityd gives non-Apple certificates a per-build `cdhash:` partition. The owner checks two consecutive builds after the 0.1.3 release. This is a post-release check, not a release gate.

## Build from source

Run `sh scripts/build-linux.sh --deb` on Linux, `scripts\build-windows.ps1` on Windows, or `sh scripts/build-macos.sh` on macOS. Prerequisites, commands, feature examples and common failures are in [Building DataTab from source](BUILDING.md).

## Remove DataTab

On Windows, use **Installed apps** in Settings. On Debian or Ubuntu, remove the package with `sudo apt remove datatab`. Removing the app does not remove your data. See [Privacy](../PRIVACY.md) for data locations and removal.
