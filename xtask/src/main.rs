//! Project automation. Run with `cargo xtask <task>` (alias in .cargo/config.toml).
//!
//! Tasks:
//!   installer   Build the release exe and package it with NSIS (Windows only).

use std::env;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

fn main() {
    let task = env::args().nth(1);
    let result = match task.as_deref() {
        Some("installer") => installer(),
        _ => {
            eprintln!("usage: cargo xtask installer");
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

fn installer() -> Result<(), String> {
    if !cfg!(windows) {
        return Err("the installer can only be built on Windows".into());
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

/// Read `version = "..."` from the `[package]` section without a TOML parser.
fn package_version(manifest: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(manifest).map_err(|e| e.to_string())?;
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
