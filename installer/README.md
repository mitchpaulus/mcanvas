# Installers

## Windows

Requires [NSIS](https://nsis.sourceforge.io/) 3.x on the build machine (`winget install NSIS.NSIS`).

```
msh windows_installer.msh
```

This wraps `cargo xtask package`, which runs `cargo build --release`, reads the version from `Cargo.toml`, and
invokes `makensis` (found on PATH or in the default NSIS install location).
Output: `installer\mcanvas-<version>-setup.exe`. The manual equivalent is
`makensis /DVERSION=<version> installer\mcanvas.nsi`. Installs per-user into
`%LOCALAPPDATA%\Programs\mcanvas`, adds a Start Menu shortcut, an
Add/Remove Programs entry, and an uninstaller. No admin prompt.

`.cargo/config.toml` statically links the MSVC C runtime so the exe does not
need `vcruntime140.dll` on the target machine.

### File association

Canvas files use the `.mc` extension. The installer registers it per-user so
double-clicking a `.mc` file opens it in mcanvas. (A double extension like
`.canvas.json` would not work: Windows keys associations on the last dot.)

### SmartScreen

The installer is unsigned, so SmartScreen shows "Windows protected your PC" on
first run. Only a code-signing certificate removes that.

## Linux

```
msh install_linux.msh
cargo xtask uninstall
```

`install_linux.msh` wraps `cargo xtask install`, which runs `cargo build --release` and installs for the current user, no
root needed:

| File | Destination |
| --- | --- |
| `target/release/mcanvas` | `~/.local/bin/mcanvas` |
| `mcanvas.desktop` | `$XDG_DATA_HOME/applications/mcanvas.desktop` |
| `mcanvas-mime.xml` | `$XDG_DATA_HOME/mime/packages/mcanvas.xml` |
| `mcanvas.svg` | `$XDG_DATA_HOME/icons/hicolor/scalable/apps/mcanvas.svg` |

`$XDG_DATA_HOME` defaults to `~/.local/share`. The desktop entry adds mcanvas
to the application menu, and the MIME definition registers `*.mc` as
`application/x-mcanvas` so double-clicking a canvas opens it. The installed
desktop entry's `Exec` line is rewritten to the absolute path of the binary, so
it works even when `~/.local/bin` is not on the desktop session's `PATH`.

Re-running `install` upgrades in place, including while mcanvas is running.
