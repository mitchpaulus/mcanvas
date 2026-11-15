//! Project automation. Run with `cargo xtask <task>` (alias in .cargo/config.toml).
//!
//! Tasks:
//!   package     Build the release exe and package it into an NSIS setup exe
//!               (Windows only).
//!   install     Build the release binary and install it for the current user,
//!               with a launcher entry and `.mc` file association (Linux only).
//!   uninstall   Remove everything `install` put in place (Linux only).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

fn main() {
    let task = env::args().nth(1);
    let result = match task.as_deref() {
        Some("package") => package(),
        Some("install") => install(),
        Some("uninstall") => uninstall(),
        _ => {
            eprintln!("usage: cargo xtask <package|install|uninstall>");
            process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn package() -> Result<(), String> {
    if !cfg!(windows) {
        return Err("package builds the Windows installer and only runs on Windows; on Linux use `cargo xtask install`".into());
    }
    let root = root();
    let version = package_version(&root.join("Cargo.toml"))?;
    let makensis = find_makensis()?;

    run(Command::new("cargo").args(["build", "--release"]).current_dir(&root))?;

    let exe = root.join("target/release/mcanvas.exe");
    if !exe.exists() {
        return Err(format!("{} not found after build", exe.display()));
    }

    run(Command::new(&makensis)
        .arg(format!("/DVERSION={version}"))
        .arg("mcanvas.nsi")
        .current_dir(root.join("installer")))?;

    println!(
        "built {}",
        root.join(format!("installer/mcanvas-{version}-setup.exe")).display()
    );
    Ok(())
}

/// Where a per-user Linux install puts its files (XDG base directories).
struct InstallPaths {
    bin: PathBuf,
    desktop: PathBuf,
    mime: PathBuf,
    icon: PathBuf,
    /// Root of the shared-mime-info database that `mime` belongs to.
    mime_db: PathBuf,
}

impl InstallPaths {
    fn new() -> Result<Self, String> {
        if !cfg!(target_os = "linux") {
            return Err("install/uninstall are Linux only; on Windows use `cargo xtask package`".into());
        }
        let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
        let data = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"));
        Ok(Self {
            bin: home.join(".local/bin/mcanvas"),
            desktop: data.join("applications/mcanvas.desktop"),
            mime: data.join("mime/packages/mcanvas.xml"),
            icon: data.join("icons/hicolor/scalable/apps/mcanvas.svg"),
            mime_db: data.join("mime"),
        })
    }

    fn files(&self) -> [&Path; 4] {
        [&self.bin, &self.desktop, &self.mime, &self.icon]
    }
}

fn install() -> Result<(), String> {
    let paths = InstallPaths::new()?;
    let root = root();

    run(Command::new("cargo").args(["build", "--release"]).current_dir(&root))?;

    // The launcher gets the absolute path: ~/.local/bin is not reliably on the
    // desktop session's PATH.
    let template = read(&root.join("installer/mcanvas.desktop"))?;
    let exec = format!("Exec={} ", exec_quote(&paths.bin));
    if !template.contains("Exec=mcanvas ") {
        return Err("installer/mcanvas.desktop has no `Exec=mcanvas ` line".into());
    }
    let desktop = template.replace("Exec=mcanvas ", &exec);

    place(&paths.bin, |tmp| fs::copy(root.join("target/release/mcanvas"), tmp).map(drop))?;
    place(&paths.desktop, |tmp| fs::write(tmp, &desktop))?;
    place(&paths.mime, |tmp| fs::copy(root.join("installer/mcanvas-mime.xml"), tmp).map(drop))?;
    place(&paths.icon, |tmp| fs::copy(root.join("installer/mcanvas.svg"), tmp).map(drop))?;
    refresh_databases(&paths);

    for f in paths.files() {
        println!("installed {}", f.display());
    }
    Ok(())
}

fn uninstall() -> Result<(), String> {
    let paths = InstallPaths::new()?;
    for f in paths.files() {
        match fs::remove_file(f) {
            Ok(()) => println!("removed {}", f.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", f.display())),
        }
    }
    refresh_databases(&paths);
    Ok(())
}

/// Create `dest` by writing a sibling temp file and renaming it over the
/// target, so a running mcanvas binary can be replaced ("text file busy"
/// otherwise) and nothing ever sees a half-written file.
fn place(dest: &Path, write: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<(), String> {
    let dir = dest.parent().unwrap();
    let tmp = dir.join(format!(".{}.tmp", dest.file_name().unwrap().to_string_lossy()));
    fs::create_dir_all(dir)
        .and_then(|()| write(&tmp))
        .and_then(|()| fs::rename(&tmp, dest))
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("{}: {e}", dest.display())
        })
}

/// Quote a path as one argument of a desktop entry `Exec` key. Backslashes
/// are doubled twice: once for the quoting rule, once for string escaping.
fn exec_quote(path: &Path) -> String {
    let mut out = String::from("\"");
    for c in path.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' => out.push_str("\\\\"),
            '\\' => out.push_str("\\\\\\"),
            '%' => out.push('%'),
            _ => {}
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Rebuild the MIME and launcher caches so the desktop notices the change.
/// Best effort: the files are in place either way and get picked up on the
/// next login.
fn refresh_databases(paths: &InstallPaths) {
    let apps = paths.desktop.parent().unwrap();
    for (tool, dir) in [("update-mime-database", paths.mime_db.as_path()), ("update-desktop-database", apps)] {
        if !dir.exists() {
            continue;
        }
        match Command::new(tool).arg(dir).status() {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("warning: {tool} failed with {s}"),
            Err(e) => eprintln!("warning: could not run {tool}: {e}"),
        }
    }
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Read `version = "..."` from the `[package]` section without a TOML parser.
fn package_version(manifest: &Path) -> Result<String, String> {
    let text = read(manifest)?;
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(rest) = line.strip_prefix("version") {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=') {
                    return Ok(rest.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    Err("no version in [package]".into())
}

fn find_makensis() -> Result<PathBuf, String> {
    if Command::new("makensis").arg("/VERSION").output().is_ok() {
        return Ok(PathBuf::from("makensis"));
    }
    let candidates = [
        env::var("ProgramFiles(x86)").ok(),
        env::var("ProgramFiles").ok(),
        env::var("LOCALAPPDATA").map(|p| format!("{p}\\Programs")).ok(),
    ];
    for base in candidates.into_iter().flatten() {
        let p = Path::new(&base).join("NSIS").join("makensis.exe");
        if p.exists() {
            return Ok(p);
        }
    }
    Err("makensis not found; install NSIS with `winget install NSIS.NSIS`".into())
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed with {status}", cmd.get_program()))
    }
}
