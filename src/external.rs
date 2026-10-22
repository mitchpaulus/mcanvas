//! Editing a node's source in an external editor.
//!
//! The source goes to a temporary `.typ` file and the editor runs on a
//! background thread. Each time the file's contents change (the user saved),
//! and once more when the editor exits, the new text is handed back to the UI
//! thread, which writes it into the node.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the file is checked for saves while the editor is open.
const POLL: Duration = Duration::from_millis(300);
/// An editor that exits sooner than this without a save probably forked into
/// the background instead of waiting for the file to be closed.
const QUICK_EXIT: Duration = Duration::from_secs(2);
/// A read that fails this many polls in a row is reported. One failure is
/// usually an editor in the middle of saving.
const READ_RETRIES: u32 = 10;

/// What the background thread reports back to the UI thread.
pub enum Event {
    /// The file was saved with this text.
    Saved(String),
    /// Something the user should know while editing goes on: the file cannot
    /// be read, or the editor returned at once and only the file is watched.
    Note(String),
    /// Editing ended; the message says how and what came back.
    Closed(String),
}

const VAR: &str = "MCANVAS_EDITOR";

/// The editor command split into program and arguments, and where it was
/// set, for messages.
pub struct Editor {
    argv: Vec<String>,
    source: &'static str,
}

/// The editor command from `MCANVAS_EDITOR`, or a message saying why there is
/// none. `VISUAL` and `EDITOR` are deliberately ignored: they usually name
/// terminal editors set up for shell use.
pub fn editor_command() -> Result<Editor, String> {
    let (value, source) = match std::env::var(VAR) {
        Ok(v) => (v, VAR),
        Err(std::env::VarError::NotUnicode(v)) => {
            return Err(format!("{VAR} is not valid text: {v:?}"));
        }
        Err(std::env::VarError::NotPresent) => match saved_setting() {
            Some(found) => found,
            None => {
                return Err(format!(
                    "{VAR} is not set, so there is no external editor; set it to a command, \
                     e.g. \"code --wait\"{}",
                    if cfg!(windows) { "" } else { ", and restart mcanvas" }
                ));
            }
        },
    };
    let argv = split_command(&value);
    if argv.is_empty() {
        return Err(format!("{source} is set but empty; set it to an editor command, e.g. \"code --wait\""));
    }
    Ok(Editor { argv, source })
}

/// A program only sees the environment it was started with, so on Windows a
/// variable set in Settings after mcanvas (or the shell that launched it)
/// started is missing. Read the saved user, then system, value instead.
#[cfg(windows)]
fn saved_setting() -> Option<(String, &'static str)> {
    saved_env(VAR).into_iter().next().map(|(value, user)| {
        let source = if user {
            "MCANVAS_EDITOR in Windows user settings"
        } else {
            "MCANVAS_EDITOR in Windows system settings"
        };
        (value, source)
    })
}

#[cfg(not(windows))]
fn saved_setting() -> Option<(String, &'static str)> {
    None
}

/// The saved user and system values of an environment variable, user first,
/// each with whether it is the user's.
#[cfg(windows)]
fn saved_env(name: &str) -> Vec<(String, bool)> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;
    [
        (HKEY_CURRENT_USER, "Environment", true),
        (HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment", false),
    ]
    .into_iter()
    .filter_map(|(root, key, user)| {
        let value: String = RegKey::predef(root).open_subkey(key).ok()?.get_value(name).ok()?;
        Some((value, user))
    })
    .collect()
}

/// Folders on the PATH saved in Windows settings. Like the editor variable,
/// mcanvas's own PATH misses folders added after it started, which is common
/// right after installing an editor.
#[cfg(windows)]
fn saved_path() -> Vec<PathBuf> {
    // Windows builds PATH as the system folders, then the user's.
    saved_env("Path")
        .into_iter()
        .rev()
        .flat_map(|(value, _)| std::env::split_paths(&expand_vars(&value)).collect::<Vec<_>>())
        .collect()
}

#[cfg(not(windows))]
fn saved_path() -> Vec<PathBuf> {
    Vec::new()
}

/// Expand `%NAME%` references the way Windows does for saved variables;
/// unknown names are kept as written.
#[cfg_attr(not(windows), allow(dead_code))]
fn expand_vars(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = &after[..end];
        match std::env::var(name) {
            Ok(v) if !name.is_empty() => out.push_str(&v),
            _ => out.push_str(&rest[start..start + end + 2]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Locate `name` the way a shell would: as a path if it contains a
/// separator, otherwise in each PATH folder, trying the PATHEXT extensions on
/// Windows when it has none. The error says what was searched.
fn find_program(name: &str, source: &str) -> Result<PathBuf, String> {
    let given = Path::new(name);
    if given.components().count() > 1 {
        return if given.is_file() {
            Ok(given.to_path_buf())
        } else if given.is_dir() {
            Err(format!("editor \"{name}\" from {source} is a folder, not a program"))
        } else {
            Err(format!("editor \"{name}\" from {source} was not found: there is no such file"))
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
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let own = dirs.len();
    for dir in saved_path() {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
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
    let saved = match dirs.len() - own {
        0 => String::new(),
        n => format!(" or the {n} more on the PATH saved in Windows settings"),
    };
    Err(format!(
        "editor \"{name}\" from {source} was not found in any of the {own} folders on \
         mcanvas's PATH{saved}; use its full path instead"
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

/// The file's text. Windows editors may add a byte order mark or save
/// UTF-16; anything else must be UTF-8.
fn decode(bytes: &[u8]) -> Result<String, String> {
    let utf16 = |b: &[u8], unit: fn([u8; 2]) -> u16| {
        if !b.len().is_multiple_of(2) {
            return Err("is UTF-16 with an odd number of bytes".to_string());
        }
        let units: Vec<u16> = b.chunks_exact(2).map(|c| unit([c[0], c[1]])).collect();
        String::from_utf16(&units).map_err(|_| "is not valid UTF-16".to_string())
    };
    if let Some(b) = bytes.strip_prefix(b"\xFF\xFE") {
        return utf16(b, u16::from_le_bytes);
    }
    if let Some(b) = bytes.strip_prefix(b"\xFE\xFF") {
        return utf16(b, u16::from_be_bytes);
    }
    let b = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    String::from_utf8(b.to_vec()).map_err(|_| "is not UTF-8 text; save it as UTF-8".to_string())
}

/// Batch files run through `cmd.exe` on Windows.
fn is_batch(program: &Path) -> bool {
    cfg!(windows)
        && program
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

fn spawn_editor(program: &Path, args: &[String], file: &Path) -> std::io::Result<Child> {
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
        if is_batch(program) {
            cmd.creation_flags(CREATE_NO_WINDOW);
        } else {
            cmd.creation_flags(CREATE_NEW_CONSOLE);
            return spawn_without_std_handles(&mut cmd);
        }
    }
    cmd.spawn()
}

/// Rust hands the child mcanvas's own standard handles. Started from a
/// shell, those are the shell's console or pipes, and a console editor would
/// read and draw there instead of in its new window. With none to hand on,
/// Windows gives the child its new console's handles, as it does when
/// mcanvas was started from Explorer.
#[cfg(windows)]
fn spawn_without_std_handles(cmd: &mut Command) -> std::io::Result<Child> {
    use std::ffi::c_void;
    extern "system" {
        fn GetStdHandle(id: u32) -> *mut c_void;
        fn SetStdHandle(id: u32, handle: *mut c_void) -> i32;
    }
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE.
    const IDS: [u32; 3] = [-10i32 as u32, -11i32 as u32, -12i32 as u32];
    let saved = IDS.map(|id| unsafe { GetStdHandle(id) });
    for id in IDS {
        unsafe { SetStdHandle(id, std::ptr::null_mut()) };
    }
    let child = cmd.spawn();
    for (id, handle) in IDS.into_iter().zip(saved) {
        unsafe { SetStdHandle(id, handle) };
    }
    child
}

/// "4.2 s", "3 min 5 s", "2 h 10 min".
fn duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{:.1} s", d.as_secs_f64())
    } else if s < 3600 {
        format!("{} min {} s", s / 60, s % 60)
    } else {
        format!("{} h {} min", s / 3600, s / 60 % 60)
    }
}

/// An editor open on a node, as the UI thread sees it.
pub struct Session {
    /// The editor's file name, e.g. `nvim.exe`.
    pub name: String,
    pub pid: u32,
    pub file: PathBuf,
    started: Instant,
    stop: Arc<AtomicBool>,
    /// When the user last asked to open the node again; for the UI to confirm
    /// a stop.
    pub asked: Option<Instant>,
}

impl Session {
    /// e.g. "nvim.exe (pid 1234) editing C:\...\ab12.typ for 3 min 5 s".
    pub fn describe(&self) -> String {
        format!(
            "{} (pid {}) editing {} for {}",
            self.name,
            self.pid,
            self.file.display(),
            duration(self.started.elapsed())
        )
    }

    /// End the edit: close the editor if it is still running, read the file
    /// one last time and report `Closed`.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Reads the file on the background thread and reports saves.
struct Watch<F> {
    file: PathBuf,
    /// The node's source when editing began, for `normalize`.
    source: String,
    /// The text the node last got from here.
    last: String,
    /// A changed text seen once, not yet reported.
    pending: Option<String>,
    saves: usize,
    failures: u32,
    /// The read problem last reported, so it is said only once.
    reported: Option<String>,
    send: F,
}

impl<F: Fn(Event)> Watch<F> {
    /// Read the file and report a save. A change is reported once it reads
    /// the same on two polls in a row, so a save still being written is not
    /// applied half done; `settled` skips that wait for the final read.
    fn poll(&mut self, settled: bool) -> Result<(), String> {
        let shown = self.file.display();
        let text = std::fs::read(&self.file)
            .map_err(|e| format!("cannot read {shown}: {e}"))
            .and_then(|b| decode(&b).map_err(|e| format!("{shown} {e}")))
            .map(|t| normalize(&t, &self.source));
        let text = match text {
            Ok(text) => text,
            Err(e) => {
                self.pending = None;
                self.failures += 1;
                if (settled || self.failures >= READ_RETRIES) && self.reported.as_ref() != Some(&e) {
                    self.reported = Some(e.clone());
                    if !settled {
                        (self.send)(Event::Note(format!("{e}; the node is not being updated")));
                    }
                }
                return Err(e);
            }
        };
        self.failures = 0;
        self.reported = None;
        if text == self.last {
            self.pending = None;
        } else if settled || self.pending.as_ref() == Some(&text) {
            self.pending = None;
            self.last = text.clone();
            self.saves += 1;
            (self.send)(Event::Saved(text));
        } else {
            self.pending = Some(text);
        }
        Ok(())
    }
}

/// How the wait on the editor ended.
enum End {
    Exited(std::process::ExitStatus),
    Lost(std::io::Error),
    Stopped,
}

/// Write `source` to a temporary file named after the node, open it with
/// the editor, and call `send` from a background thread for every save, for
/// problems along the way, and once when editing ends. `send` must forward
/// events to the UI thread itself. Returns the session and a status message
/// saying what was started.
pub fn open(
    editor: Editor,
    node_id: &str,
    source: String,
    send: impl Fn(Event) + Send + 'static,
) -> Result<(Session, String), String> {
    let Editor { argv, source: setting } = editor;
    let program = find_program(&argv[0], setting)?;
    let dir = std::env::temp_dir().join("mcanvas");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let file = dir.join(format!("{node_id}.typ"));
    std::fs::write(&file, &source).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    let mut child = spawn_editor(&program, &argv[1..], &file).map_err(|e| {
        let _ = std::fs::remove_file(&file);
        format!("found editor {} (from {setting}) but could not start it: {e}", program.display())
    })?;
    let name = program
        .file_name()
        .map_or_else(|| program.display().to_string(), |n| n.to_string_lossy().into_owned());
    let pid = child.id();
    let via = if is_batch(&program) { ", run by cmd.exe with no window" } else { "" };
    let message = format!(
        "opened {} (pid {pid}{via}) on {}; each save updates the node",
        program.display(),
        file.display()
    );
    let stop = Arc::new(AtomicBool::new(false));
    let session = Session {
        name: name.clone(),
        pid,
        file: file.clone(),
        started: Instant::now(),
        stop: stop.clone(),
        asked: None,
    };
    let who = format!("{name} (pid {pid})");
    let mut watch = Watch {
        file,
        last: source.clone(),
        source,
        pending: None,
        saves: 0,
        failures: 0,
        reported: None,
        send,
    };
    std::thread::spawn(move || {
        let started = Instant::now();
        let stopped = || stop.load(Ordering::Relaxed);
        let end = loop {
            if stopped() {
                break End::Stopped;
            }
            match child.try_wait() {
                Ok(Some(status)) => break End::Exited(status),
                Ok(None) => {}
                Err(e) => break End::Lost(e),
            }
            let _ = watch.poll(false);
            std::thread::sleep(POLL);
        };
        if let End::Stopped = end {
            let _ = child.kill();
            let _ = child.wait();
        }
        // A save just before the editor exited has been read only once.
        let _ = watch.poll(true);
        let took = duration(started.elapsed());
        let shown = watch.file.display().to_string();
        let mut note = match end {
            End::Stopped => format!("stopped {who} after {took}; changes not saved in it are lost"),
            End::Lost(e) => format!("lost track of {who}: {e}"),
            End::Exited(s) if !s.success() => format!("{who} exited with {s} after {took}"),
            End::Exited(_) if watch.saves == 0 && started.elapsed() < QUICK_EXIT => {
                // The editor probably handed the file to a window that is
                // already open. Keep reading the file so saves there still
                // arrive, until the user stops it.
                (watch.send)(Event::Note(format!(
                    "{who} returned after {took} without saving, so it may have passed the file \
                     to another window; still watching {shown} (Edit in External Editor again \
                     stops). If it runs in the background, add its wait flag (code --wait, \
                     subl -w, gvim -f)"
                )));
                while !stopped() {
                    let _ = watch.poll(false);
                    std::thread::sleep(POLL);
                }
                format!("stopped watching {shown} after {took}")
            }
            End::Exited(_) => format!("{who} closed after {took}"),
        };
        let read = watch.poll(true);
        note.push_str(&match watch.saves {
            0 => "; no saves".to_string(),
            1 => "; 1 save applied".to_string(),
            n => format!("; {n} saves applied"),
        });
        match read {
            // Keep the file when it could not be read, so nothing is lost.
            Err(e) => note.push_str(&format!("; {e}, so the file is kept")),
            Ok(()) => {
                if let Err(e) = std::fs::remove_file(&watch.file) {
                    note.push_str(&format!("; could not delete {shown}: {e}"));
                }
            }
        }
        (watch.send)(Event::Closed(note));
    });
    Ok((session, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    fn editor(argv: &[&str]) -> Editor {
        Editor { argv: argv.iter().map(|s| s.to_string()).collect(), source: VAR }
    }

    /// Collect saves and notes until `Closed`, returning them and its message.
    fn run(rx: &Receiver<Event>) -> (Vec<String>, Vec<String>, String) {
        let (mut saved, mut notes) = (Vec::new(), Vec::new());
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                Event::Saved(t) => saved.push(t),
                Event::Note(n) => notes.push(n),
                Event::Closed(n) => return (saved, notes, n),
            }
        }
    }

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
        let script = r#"sleep 0.8; printf 'one\n' > "$0"; sleep 0.8; printf 'two\n' > "$0""#;
        let (tx, rx) = std::sync::mpsc::channel();
        let (session, message) =
            open(editor(&["sh", "-c", script]), "test-node", "zero".into(), move |e| tx.send(e).unwrap()).unwrap();
        assert_eq!(session.name, "sh");
        assert!(message.contains(&format!("(pid {})", session.pid)), "{message}");
        let (saved, notes, closed) = run(&rx);
        assert_eq!(saved, ["one", "two"]);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(closed.starts_with(&format!("sh (pid {}) closed after", session.pid)), "{closed}");
        assert!(closed.ends_with("; 2 saves applied"), "{closed}");
        assert!(!session.file.exists());
    }

    #[cfg(unix)]
    #[test]
    fn failing_editor_reports_its_exit_status() {
        let (tx, rx) = std::sync::mpsc::channel();
        open(editor(&["sh", "-c", "exit 3"]), "test-fail", "x".into(), move |e| tx.send(e).unwrap()).unwrap();
        let (saved, _, closed) = run(&rx);
        assert!(saved.is_empty());
        assert!(closed.contains("exited with exit status: 3"), "{closed}");
    }

    /// An editor that returns at once leaves the file watched until stopped.
    #[cfg(unix)]
    #[test]
    fn quick_exit_keeps_watching_until_stopped() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (session, _) =
            open(editor(&["true"]), "test-quick", "a".into(), move |e| tx.send(e).unwrap()).unwrap();
        let Event::Note(note) = rx.recv_timeout(Duration::from_secs(10)).unwrap() else { panic!() };
        assert!(note.contains("still watching"), "{note}");
        std::fs::write(&session.file, "b\n").unwrap();
        let Event::Saved(text) = rx.recv_timeout(Duration::from_secs(10)).unwrap() else { panic!() };
        assert_eq!(text, "b");
        session.stop();
        let (_, _, closed) = run(&rx);
        assert!(closed.starts_with("stopped watching"), "{closed}");
        assert!(closed.ends_with("; 1 save applied"), "{closed}");
        assert!(!session.file.exists());
    }

    /// Stopping a running editor ends it and still applies its last save.
    #[cfg(unix)]
    #[test]
    fn stop_ends_a_running_editor() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (session, _) = open(
            editor(&["sh", "-c", r#"printf 'b' > "$0"; sleep 30"#]),
            "test-stop",
            "a".into(),
            move |e| tx.send(e).unwrap(),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        session.stop();
        let (saved, _, closed) = run(&rx);
        assert_eq!(saved, ["b"]);
        assert!(closed.starts_with(&format!("stopped sh (pid {})", session.pid)), "{closed}");
    }

    /// A file that cannot be decoded is reported and kept.
    #[cfg(unix)]
    #[test]
    fn unreadable_save_is_kept() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (session, _) = open(
            editor(&["sh", "-c", r#"printf '\377\377' > "$0"; sleep 4"#]),
            "test-bad",
            "a".into(),
            move |e| tx.send(e).unwrap(),
        )
        .unwrap();
        let (saved, notes, closed) = run(&rx);
        assert!(saved.is_empty());
        assert!(notes.len() == 1 && notes[0].contains("is not UTF-8 text"), "{notes:?}");
        assert!(closed.ends_with("is not UTF-8 text; save it as UTF-8, so the file is kept"), "{closed}");
        assert!(session.file.exists());
        std::fs::remove_file(&session.file).unwrap();
    }

    /// A stand-in batch launcher, which runs hidden through `cmd.exe`.
    #[cfg(windows)]
    #[test]
    fn batch_editor_saves() {
        let script = std::env::temp_dir().join("mcanvas-test-editor.cmd");
        std::fs::write(&script, "@echo off\r\nping -n 2 127.0.0.1 >nul\r\n>\"%~1\" echo one\r\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (session, message) = open(
            editor(&[script.to_str().unwrap()]),
            "test-batch",
            "zero".into(),
            move |e| tx.send(e).unwrap(),
        )
        .unwrap();
        assert!(message.contains("run by cmd.exe"), "{message}");
        let (saved, _, closed) = run(&rx);
        assert_eq!(saved, ["one"]);
        assert!(closed.ends_with("; 1 save applied"), "{closed}");
        assert!(!session.file.exists());
        let _ = std::fs::remove_file(&script);
    }

    #[test]
    fn find_program_explains_what_it_searched() {
        let e = find_program("no-such-editor-xyz", VAR).unwrap_err();
        assert!(e.contains("\"no-such-editor-xyz\" from MCANVAS_EDITOR was not found in any of"), "{e}");
        let e = find_program("/no/such/dir/ed", VAR).unwrap_err();
        assert!(e.contains("there is no such file"), "{e}");
        let dir = std::env::temp_dir();
        let e = find_program(dir.join(".").to_str().unwrap(), VAR).unwrap_err();
        assert!(e.contains("is a folder"), "{e}");
        #[cfg(unix)]
        assert_eq!(find_program("sh", VAR).unwrap().file_name().unwrap(), "sh");
        #[cfg(windows)]
        assert!(find_program("cmd", VAR).unwrap().to_string_lossy().to_lowercase().ends_with("cmd.exe"));
    }

    #[test]
    fn normalize_drops_added_newline_and_crlf() {
        assert_eq!(normalize("a\r\nb\r\n", "a\nb"), "a\nb");
        assert_eq!(normalize("a\n", "a\n"), "a\n");
        assert_eq!(normalize("a\n\n", "a"), "a\n");
        assert_eq!(normalize("a", "a"), "a");
    }

    #[test]
    fn decode_handles_byte_order_marks() {
        assert_eq!(decode(b"\xEF\xBB\xBFhi").unwrap(), "hi");
        assert_eq!(decode(b"\xFF\xFEh\0i\0").unwrap(), "hi");
        assert_eq!(decode(b"\xFE\xFF\0h\0i").unwrap(), "hi");
        assert!(decode(b"\xFF\xFEh").unwrap_err().contains("odd number"));
        assert!(decode(b"\xC3").unwrap_err().contains("not UTF-8"));
    }

    #[test]
    fn expand_vars_keeps_unknown_names() {
        let path = std::env::var("PATH").unwrap();
        assert_eq!(expand_vars("%PATH%;x"), format!("{path};x"));
        assert_eq!(expand_vars("a%NO_SUCH_VAR_XYZ%b"), "a%NO_SUCH_VAR_XYZ%b");
        assert_eq!(expand_vars("50% off"), "50% off");
        assert_eq!(expand_vars("%%"), "%%");
    }
}
