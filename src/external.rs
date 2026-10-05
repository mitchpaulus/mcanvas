//! Editing a node's source in an external editor.
//!
//! The source goes to a temporary `.typ` file and the editor runs on a
//! background thread. Each time the file's contents change (the user saved),
//! and once more when the editor exits, the new text is handed back to the UI
//! thread, which writes it into the node.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// How often the file is checked for saves while the editor is open.
const POLL: Duration = Duration::from_millis(300);
/// An editor that exits sooner than this without a save probably forked into
/// the background instead of waiting for the file to be closed.
const QUICK_EXIT: Duration = Duration::from_secs(2);

/// What the background thread reports back to the UI thread.
pub enum Event {
    /// The file was saved with this text.
    Saved(String),
    /// The editor exited. The string explains a failure or a likely problem,
    /// and is empty when nothing went wrong.
    Closed(String),
}

const VAR: &str = "MCANVAS_EDITOR";

/// The editor command from `MCANVAS_EDITOR`, split into program and
/// arguments, or a message saying why there is none. `VISUAL` and `EDITOR`
/// are deliberately ignored: they usually name terminal editors set up for
/// shell use.
pub fn editor_command() -> Result<Vec<String>, String> {
    let Some(value) = std::env::var(VAR).ok().or_else(saved_setting) else {
        return Err(format!(
            "{VAR} is not set, so there is no external editor; set it to a command, \
             e.g. \"code --wait\"{}",
            if cfg!(windows) { "" } else { ", and restart mcanvas" }
        ));
    };
    let argv = split_command(&value);
    if argv.is_empty() {
        return Err(format!("{VAR} is set but empty; set it to an editor command, e.g. \"code --wait\""));
    }
    Ok(argv)
}

/// A program only sees the environment it was started with, so on Windows a
/// variable set in Settings after mcanvas (or the shell that launched it)
/// started is missing. Read the saved user, then machine, value instead.
#[cfg(windows)]
fn saved_setting() -> Option<String> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;
    [
        (HKEY_CURRENT_USER, "Environment"),
        (HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"),
    ]
    .into_iter()
    .find_map(|(root, key)| RegKey::predef(root).open_subkey(key).ok()?.get_value(VAR).ok())
}

#[cfg(not(windows))]
fn saved_setting() -> Option<String> {
    None
}

/// Locate `name` the way a shell would: as a path if it contains a
/// separator, otherwise in each PATH folder, trying the PATHEXT extensions on
/// Windows when it has none. The error says what was searched.
fn find_program(name: &str) -> Result<PathBuf, String> {
    let given = Path::new(name);
    if given.components().count() > 1 {
        return if given.is_file() {
            Ok(given.to_path_buf())
        } else {
            Err(format!("editor \"{name}\" from {VAR} was not found: there is no such file"))
        };
    }
    let exts: Vec<OsString> = if cfg!(windows) && given.extension().is_none() {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(OsString::from)
            .collect()
    } else {
        vec![OsString::new()]
    };
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for dir in &dirs {
        for ext in &exts {
            let mut file = OsString::from(name);
            file.push(ext);
            let candidate = dir.join(file);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(format!(
        "editor \"{name}\" from {VAR} was not found in any of the {} folders on mcanvas's PATH; \
         use its full path instead",
        dirs.len()
    ))
}

/// Split a command line on whitespace. Single or double quotes group words
/// with spaces in them. Backslashes are literal, so Windows paths work.
pub fn split_command(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = None;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            None => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(word);
    }
    out
}

/// Editors add a final newline the node did not have; drop it so an
/// unchanged file reads back as unchanged. Windows line endings become `\n`.
fn normalize(text: &str, original: &str) -> String {
    let text = text.replace("\r\n", "\n");
    if !original.ends_with('\n') {
        if let Some(t) = text.strip_suffix('\n') {
            return t.to_string();
        }
    }
    text
}

fn spawn_editor(program: &Path, args: &[String], file: &Path) -> std::io::Result<std::process::Child> {
    let mut cmd = Command::new(program);
    cmd.args(args).arg(file);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // mcanvas has no console, so a console editor such as nvim.exe needs
        // its own window; GUI programs ignore the flag. Batch launchers such
        // as VS Code's `code.cmd` only start a GUI, so hide their console.
        let batch = program
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
        cmd.creation_flags(if batch { CREATE_NO_WINDOW } else { CREATE_NEW_CONSOLE });
    }
    cmd.spawn()
}

/// Write `source` to a temporary file named after the node, open it with
/// `argv`, and call `send` from a background thread for every save and when
/// the editor exits. `send` must forward events to the UI thread itself.
/// Returns the editor program's full path.
pub fn open(
    argv: Vec<String>,
    node_id: &str,
    source: String,
    send: impl Fn(Event) + Send + 'static,
) -> Result<PathBuf, String> {
    let program = find_program(&argv[0])?;
    let dir = std::env::temp_dir().join("mcanvas");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let file = dir.join(format!("{node_id}.typ"));
    std::fs::write(&file, &source).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    let mut child = spawn_editor(&program, &argv[1..], &file).map_err(|e| {
        let _ = std::fs::remove_file(&file);
        format!("found editor {} but could not start it: {e}", program.display())
    })?;
    let shown = program.display().to_string();
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut last = source.clone();
        let mut saved = false;
        let mut check = |last: &mut String| {
            if let Ok(raw) = std::fs::read_to_string(&file) {
                let text = normalize(&raw, &source);
                if text != *last {
                    *last = text.clone();
                    saved = true;
                    send(Event::Saved(text));
                }
            }
        };
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {}
                Err(e) => break Err(e),
            }
            check(&mut last);
            std::thread::sleep(POLL);
        };
        check(&mut last);
        let note = match status {
            Err(e) => format!("lost track of editor {shown}: {e}"),
            Ok(s) if !s.success() => format!("editor {shown} exited with {s}"),
            Ok(_) if !saved && started.elapsed() < QUICK_EXIT => {
                "editor returned at once; if it runs in the background, add its wait flag \
                 (code --wait, subl -w, gvim -f)"
                    .into()
            }
            Ok(_) => String::new(),
        };
        let _ = std::fs::remove_file(&file);
        send(Event::Closed(note));
    });
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_handles_quotes_and_windows_paths() {
        assert_eq!(split_command("code --wait"), ["code", "--wait"]);
        assert_eq!(
            split_command(r#""C:\Program Files\Editor\ed.exe" -w"#),
            [r"C:\Program Files\Editor\ed.exe", "-w"]
        );
        assert_eq!(split_command("  gvim  -f 'a b' "), ["gvim", "-f", "a b"]);
        assert_eq!(split_command(r#"ed """#), ["ed", ""]);
        assert!(split_command("   ").is_empty());
    }

    /// A stand-in editor that saves twice and exits; the file path arrives as `$0`.
    #[cfg(unix)]
    #[test]
    fn saves_and_exit_are_reported() {
        let script = r#"sleep 0.5; printf 'one\n' > "$0"; sleep 0.5; printf 'two\n' > "$0""#;
        let argv = vec!["sh".into(), "-c".into(), script.into()];
        let (tx, rx) = std::sync::mpsc::channel();
        open(argv, "test-node", "zero".into(), move |e| tx.send(e).unwrap()).unwrap();
        let mut seen = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                Event::Saved(t) => seen.push(t),
                Event::Closed(note) => {
                    assert_eq!(note, "");
                    break;
                }
            }
        }
        assert_eq!(seen, ["one", "two"]);
        assert!(!std::env::temp_dir().join("mcanvas/test-node.typ").exists());
    }

    #[test]
    fn find_program_explains_what_it_searched() {
        let e = find_program("no-such-editor-xyz").unwrap_err();
        assert!(e.contains("\"no-such-editor-xyz\" from MCANVAS_EDITOR was not found in any of"), "{e}");
        let e = find_program("/no/such/dir/ed").unwrap_err();
        assert!(e.contains("there is no such file"), "{e}");
        #[cfg(unix)]
        assert_eq!(find_program("sh").unwrap().file_name().unwrap(), "sh");
    }

    #[test]
    fn normalize_drops_added_newline_and_crlf() {
        assert_eq!(normalize("a\r\nb\r\n", "a\nb"), "a\nb");
        assert_eq!(normalize("a\n", "a\n"), "a\n");
        assert_eq!(normalize("a\n\n", "a"), "a\n");
        assert_eq!(normalize("a", "a"), "a");
    }
}
