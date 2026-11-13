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

The release has a `DataTab.app` for Intel and one for Apple Silicon. Both are unsigned and need macOS 11 or later. Drag the app to `/Applications`. A downloaded app needs its quarantine flag cleared before its first start:

```bash
xattr -dr com.apple.quarantine /Applications/DataTab.app
```

## Build from source

Run `sh scripts/build-linux.sh --deb` on Linux, `scripts\build-windows.ps1` on Windows, or `sh scripts/build-macos.sh` on macOS. Prerequisites, commands, feature examples and common failures are in [Building DataTab from source](BUILDING.md).

## Remove DataTab

On Windows, use **Installed apps** in Settings. On Debian or Ubuntu, remove the package with `sudo apt remove datatab`. Removing the app does not remove your data. See [Privacy](../PRIVACY.md) for data locations and removal.
