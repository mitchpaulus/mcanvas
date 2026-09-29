# Windows installer

Requires [NSIS](https://nsis.sourceforge.io/) 3.x on the build machine (`winget install NSIS.NSIS`).

```
cargo xtask installer
```

This runs `cargo build --release`, reads the version from `Cargo.toml`, and
invokes `makensis` (found on PATH or in the default NSIS install location).
Output: `installer\mcanvas-<version>-setup.exe`. The manual equivalent is
`makensis /DVERSION=<version> installer\mcanvas.nsi`. Installs per-user into
`%LOCALAPPDATA%\Programs\mcanvas`, adds a Start Menu shortcut, an
Add/Remove Programs entry, and an uninstaller. No admin prompt.

`.cargo/config.toml` statically links the MSVC C runtime so the exe does not
need `vcruntime140.dll` on the target machine.

## File association

Canvas files use the `.mc` extension. The installer registers it per-user so
double-clicking a `.mc` file opens it in mcanvas. (A double extension like
`.canvas.json` would not work: Windows keys associations on the last dot.)

## SmartScreen

The installer is unsigned, so SmartScreen shows "Windows protected your PC" on
first run. Only a code-signing certificate removes that.
