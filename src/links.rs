//! Which clicked links may be handed to the OS, and what a link really points
//! to (for the hover tooltip).
//!
//! `http`, `https` and `mailto` open as they are. `file:` links open only when
//! they name a local path (no remote host, which on Windows would connect to an
//! SMB share and send the user's NTLM hash) and the target is not something the
//! OS would run (executables, scripts, shortcuts, installers). The extension
//! blocklist cannot be complete, so the hover shows the real target and the
//! click checks the file itself too: files with no extension, Unix files with an
//! execute bit, and symlinks whose target is blocked are refused.

use std::path::{Path, PathBuf};

/// Where a link goes once it has passed the checks that need no file access.
#[derive(Debug, PartialEq)]
pub enum Target {
    /// `http`, `https` or `mailto`: passed to the OS unchanged.
    Web(String),
    /// A decoded local path from a `file:` link.
    File(PathBuf),
}

/// File types the OS would run, install, mount, or follow elsewhere rather
/// than open as a document. Lowercase, without the dot.
const BLOCKED_EXTENSIONS: &[&str] = &[
    // Windows executables and libraries.
    "exe", "com", "scr", "pif", "cpl", "msc", "dll", "ocx", "sys", "drv",
    // Windows scripts and script hosts.
    "bat", "cmd", "ps1", "psm1", "psd1", "ps1xml", "ps2", "psc1", "vbs", "vbe", "js",
    "jse", "wsf", "wsh", "wsc", "ws", "hta", "sct",
    // Installers, packages, and things that run on open.
    "msi", "msp", "mst", "msix", "msixbundle", "appx", "appxbundle", "application",
    "appref-ms", "xbap", "gadget", "jar", "jnlp", "reg", "inf", "chm", "diagcab",
    // Shortcuts and shell links (they point elsewhere, possibly at a program).
    "lnk", "url", "scf", "website", "settingcontent-ms", "library-ms", "search-ms",
    "searchconnector-ms",
    // Disk images (opening one mounts it and can autorun its contents).
    "iso", "img", "vhd", "vhdx",
    // macOS.
    "app", "command", "tool", "workflow", "pkg", "mpkg", "dmg", "terminal",
    // Linux and Unix.
    "sh", "bash", "zsh", "csh", "ksh", "fish", "desktop", "appimage", "run", "bin",
    "deb", "rpm", "flatpakref", "snap",
    // Interpreters commonly associated with "open" on desktops.
    "py", "pyw", "pyz", "pyzw", "pyc", "pl", "rb", "php", "lua", "tcl",
];

/// Check a link without touching the file system. Errors explain why the link
/// will not open.
pub fn classify(url: &str) -> Result<Target, String> {
    let lower = url.to_ascii_lowercase();
    if ["http://", "https://", "mailto:"].iter().any(|p| lower.starts_with(p)) {
        return Ok(Target::Web(url.to_string()));
    }
    if !lower.starts_with("file:") {
        let scheme = url.split(':').next().unwrap_or(url);
        return Err(format!(
            "\"{scheme}:\" links do not open; only http, https, mailto and local file links do"
        ));
    }
    let rest = url["file:".len()..]
        .strip_prefix("//")
        .ok_or("write file links as file:///path")?;
    // The host runs up to the next slash. Only an empty host or localhost is
    // this machine; anything else is a network share.
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return Err(format!("links to network locations do not open (host \"{host}\")"));
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let path = percent_decode(path).ok_or("the path is not valid UTF-8 after decoding")?;
    if path.is_empty() {
        return Err("the link has no path".into());
    }
    if path.contains('\0') {
        return Err("the path contains a NUL character".into());
    }
    // `file:////server/share` and `file:///\\server` are UNC paths on Windows.
    if path.starts_with("//") || path.starts_with("/\\") || path.starts_with('\\') {
        return Err("links to network locations do not open (UNC path)".into());
    }
    // `/C:/dir` is a Windows drive path; drop the leading slash.
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    let path = if drive { path[1..].to_string() } else { path };
    // A colon past the drive letter would name an NTFS alternate data stream
    // (`report.txt:evil.exe`), which hides the real type.
    let after_drive = if drive { &path[2..] } else { &path[..] };
    if after_drive.contains(':') {
        return Err("the path contains a colon".into());
    }
    if let Some(ext) = blocked_extension(&path) {
        return Err(format!("\".{ext}\" files can run programs, so they do not open from a link"));
    }
    #[cfg(windows)]
    let path = path.replace('/', "\\");
    Ok(Target::File(PathBuf::from(path)))
}

/// Checks that need the file itself. Directories open in the file manager.
pub fn check_file(path: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    // A symlink or junction named `notes.txt` may lead to a program. Checked
    // before the directory case because some bundles (`.app`) are directories.
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(ext) = blocked_extension(&real.to_string_lossy()) {
        return Err(format!(
            "{} leads to a \".{ext}\" file, which can run programs",
            path.display()
        ));
    }
    if meta.is_dir() {
        return Ok(());
    }
    if extension(&real.to_string_lossy()).is_none() {
        return Err(format!(
            "{} has no file extension, so it is not clear what opening it would do",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 != 0 {
            return Err(format!("{} is marked executable", path.display()));
        }
    }
    Ok(())
}

/// What the tooltip shows: the URL for web links, the decoded path for file
/// links (so `%2E` tricks and look-alike link text are visible), and the
/// reason when the link will not open.
pub fn describe(url: &str) -> (String, String) {
    match classify(url) {
        Ok(Target::Web(u)) => (u, String::new()),
        Ok(Target::File(p)) => (p.display().to_string(), String::new()),
        Err(why) => (url.to_string(), why),
    }
}

/// Last path component's extension, lowercase. Windows ignores trailing dots
/// and spaces (`setup.exe.` runs setup.exe), so they are dropped first, as are
/// trailing slashes (a macOS `Name.app/` bundle is a directory that launches).
fn extension(path: &str) -> Option<String> {
    let path = path.trim_end_matches(['/', '\\']);
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let name = name.trim_end_matches(['.', ' ']);
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

fn blocked_extension(path: &str) -> Option<String> {
    extension(path).filter(|e| BLOCKED_EXTENSIONS.contains(&e.as_str()))
}

/// Decode `%XX` escapes. None if the result is not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(url: &str) -> String {
        match classify(url) {
            Ok(Target::File(p)) => p.to_string_lossy().replace('\\', "/"),
            other => panic!("{url}: {other:?}"),
        }
    }

    fn refused(url: &str) -> String {
        classify(url).expect_err(url)
    }

    #[test]
    fn web_links_pass_through() {
        assert_eq!(classify("https://a.b/c?d#e"), Ok(Target::Web("https://a.b/c?d#e".into())));
        assert_eq!(classify("HTTP://x"), Ok(Target::Web("HTTP://x".into())));
        assert!(matches!(classify("mailto:a@b.c"), Ok(Target::Web(_))));
        assert!(refused("ms-settings:privacy").contains("ms-settings:"));
        assert!(refused("javascript:alert(1)").contains("javascript:"));
    }

    #[test]
    fn local_file_paths() {
        assert_eq!(file("file:///home/me/notes.pdf"), "/home/me/notes.pdf");
        assert_eq!(file("file://localhost/home/me/a.txt"), "/home/me/a.txt");
        assert_eq!(file("FILE:///C:/Users/me/My%20Doc.docx"), "C:/Users/me/My Doc.docx");
        assert_eq!(file("file:///C:/projects/"), "C:/projects/");
        assert_eq!(file("file:///tmp/a.txt?x=1#frag"), "/tmp/a.txt");
    }

    #[test]
    fn network_locations_refused() {
        assert!(refused("file://server/share/a.txt").contains("server"));
        assert!(refused("file:////server/share/a.txt").contains("UNC"));
        assert!(refused("file:///\\\\server\\share\\a.txt").contains("UNC"));
        assert!(refused("file:///%5C%5Cserver/a.txt").contains("UNC"));
        assert!(refused("file:/C:/a.txt").contains("file:///"));
    }

    #[test]
    fn programs_refused() {
        for url in [
            "file:///C:/Users/me/Downloads/setup.exe",
            "file:///C:/x/RUN.BAT",
            "file:///C:/x/a.lnk",
            "file:///C:/x/setup.exe.",
            "file:///C:/x/setup.exe%20%20",
            "file:///C:/x/setup%2Eexe",
            "file:///home/me/install.sh",
            "file:///home/me/app.desktop",
            "file:///Applications/Evil.app",
            "file:///Applications/Evil.app/",
        ] {
            let why = refused(url);
            assert!(why.contains("can run programs") || why.contains("colon"), "{url}: {why}");
        }
        assert!(refused("file:///C:/x/notes.txt:evil.exe").contains("colon"));
    }

    #[test]
    fn describe_shows_decoded_target_and_reason() {
        assert_eq!(describe("https://x.y"), ("https://x.y".into(), String::new()));
        let (target, why) = describe("file:///C:/a%20b/c.pdf");
        assert_eq!(target.replace('\\', "/"), "C:/a b/c.pdf");
        assert!(why.is_empty());
        let (_, why) = describe("file:///C:/a/b.exe");
        assert!(why.contains(".exe"));
    }

    #[test]
    fn file_checks() {
        let dir = std::env::temp_dir().join(format!("mcanvas-links-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let doc = dir.join("a.txt");
        std::fs::write(&doc, "x").unwrap();
        assert_eq!(check_file(&doc), Ok(()));
        assert_eq!(check_file(&dir), Ok(()));
        let bare = dir.join("noext");
        std::fs::write(&bare, "x").unwrap();
        assert!(check_file(&bare).unwrap_err().contains("no file extension"));
        assert!(check_file(&dir.join("missing.txt")).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let exe = dir.join("tool.txt");
            std::fs::write(&exe, "x").unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(check_file(&exe).unwrap_err().contains("executable"));
            let target = dir.join("real.sh");
            std::fs::write(&target, "x").unwrap();
            let link = dir.join("innocent.txt");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(check_file(&link).unwrap_err().contains(".sh"));
            let bundle = dir.join("Thing.app");
            std::fs::create_dir(&bundle).unwrap();
            let dir_link = dir.join("folder");
            std::os::unix::fs::symlink(&bundle, &dir_link).unwrap();
            assert!(check_file(&dir_link).unwrap_err().contains(".app"));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
