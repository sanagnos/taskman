//! The private file the account form can save a pasted token to where there's no keychain (WSL, a
//! server): one file per account in a `tokens` folder beside config.json, named by the account's
//! id (`jira-1`). The folder is the user's alone (0700) and the file too (0600), both from the
//! moment they're made. The account saves the command that reads it, `cmd:cat '<path>'`, so
//! config.json still holds only where to read a token from. See "Tokens in a private file" in
//! docs/design.md.

use std::fs::{File, Metadata, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// The folder the token files are in: beside config.json, wherever `TASKMAN_CONFIG` or
/// `XDG_CONFIG_HOME` put that; in unit tests, beside the scratch config.
pub fn dir() -> PathBuf {
    let config = crate::store::config_path();
    config.parent().unwrap_or(Path::new(".")).join("tokens")
}

/// The file `name` is saved in.
pub fn path(name: &str) -> PathBuf {
    dir().join(name)
}

/// The folder, as the user would type it, for the form's hints.
pub fn shown_dir() -> String {
    // A unit test's is a scratch folder: named as the usual one, so screens draw the same.
    if cfg!(test) {
        return "~/.config/taskman/tokens/".into();
    }
    format!("{}/", crate::store::tilde(&dir()))
}

/// Names are keychain names (account ids) that don't start with `.`: no `/`, no `..`, and none
/// of the temporary files written on the way.
pub fn is_name(name: &str) -> bool {
    crate::keychain::is_name(name) && !name.starts_with('.')
}

/// The source that reads file `name`: `cat` and its path as one shell word, whatever the path
/// holds.
pub fn source(name: &str) -> String {
    format!("cmd:cat '{}'", path(name).display().to_string().replace('\'', r"'\''"))
}

/// The file `source` reads, if it's the source `source` makes for one.
pub fn name_of(source: &str) -> Option<&str> {
    let source = source.trim();
    let name = source.strip_suffix('\'')?.rsplit('/').next()?;
    (is_name(name) && self::source(name) == source).then_some(name)
}

/// Why the folder or file at `path` (`meta`, read without following a link) can't hold a token of
/// the user's, if it can't, and how to fix that.
fn refused(path: &Path, meta: &Metadata, dir: bool) -> Option<String> {
    let shown = crate::store::tilde(path);
    let what = if dir { "a folder" } else { "a file" };
    let again = "delete it, then paste the token again";
    // SAFETY: geteuid takes nothing and can't fail; it reads or writes no memory of ours.
    let me = unsafe { libc::geteuid() };
    Some(match () {
        _ if meta.file_type().is_symlink() => format!("{shown} is a link, not {what}: {again}"),
        _ if dir && !meta.is_dir() || !dir && !meta.is_file() => {
            format!("{shown} isn't {what}: {again}")
        }
        _ if meta.uid() != me => format!("{shown} belongs to another user: {again}"),
        _ if dir && meta.mode() & 0o077 != 0 => {
            format!("other users can open {shown}: run chmod 700 {shown}")
        }
        _ => return None,
    })
}

/// Saves `token` as file `name`, replacing one saved before: written to a file beside it, created
/// 0600 and never through a link, then renamed over it, so it's never half written or readable by
/// others even for a moment. Errors never hold the token.
#[cfg(test)]
pub fn store(name: &str, token: &str) -> Result<(), String> {
    store_in(&dir(), name, token)
}

/// `store` in folder `dir`, the `dir()` of the thread that made the account's source: a unit
/// test's background thread has a scratch folder of its own.
pub fn store_in(dir: &Path, name: &str, token: &str) -> Result<(), String> {
    crate::keychain::check_token(token)?;
    if !is_name(name) {
        return Err(format!("{name} can't name a token file"));
    }
    make_dir(dir)?;
    let (path, shown) = (dir.join(name), crate::store::tilde(&dir.join(name)));
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if !meta.is_file() => return Err(refused(&path, &meta, false).unwrap_or_default()),
        _ => {}
    }
    let tmp = dir.join(format!(".{name}.tmp"));
    // A file left by a save that stopped part-way; create_new won't take its place.
    let _ = std::fs::remove_file(&tmp);
    let written = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        f.write_all(token.as_bytes())?;
        f.write_all(b"\n")?;
        // A system that can't sync a file (some network and virtual filesystems) still keeps it.
        if let Err(e) = f.sync_all()
            && e.kind() != std::io::ErrorKind::Unsupported
            && e.raw_os_error() != Some(libc::EINVAL)
        {
            return Err(e);
        }
        drop(f);
        std::fs::rename(&tmp, &path)?;
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    written.map_err(|e: std::io::Error| {
        let _ = std::fs::remove_file(&tmp);
        format!("couldn't save the token to {shown}: {e}")
    })
}

/// Makes the folder, the user's alone from the start; one there already must be the user's alone
/// too, and a folder rather than a link to one.
fn make_dir(dir: &Path) -> Result<(), String> {
    let failed = |d: &Path, e| format!("couldn't make {}: {e}", crate::store::tilde(d));
    if let Some(parent) = dir.parent() {
        // The config folder, made private as config.json's save makes it.
        let made = match crate::store::own_config_dir() {
            Some(own) if own == parent => crate::store::private_dir(parent),
            _ => std::fs::create_dir_all(parent),
        };
        made.map_err(|e| failed(parent, e))?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(failed(dir, e)),
    }
    let meta = std::fs::symlink_metadata(dir).map_err(|e| failed(dir, e))?;
    refused(dir, &meta, true).map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().mode() & 0o777
    }

    #[test]
    fn lives_beside_config_json() {
        assert_eq!(path("jira-1"), crate::store::config_path().with_file_name("tokens/jira-1"));
        assert!(shown_dir().ends_with("/tokens/"));
    }

    #[test]
    fn the_source_is_cat_of_the_path_quoted() {
        let s = source("jira-1");
        assert_eq!(s, format!("cmd:cat '{}'", path("jira-1").display()));
        assert_eq!(name_of(&s), Some("jira-1"));
        assert_eq!(name_of(&format!(" {s} ")), Some("jira-1"));
        for other in [
            "cmd:cat ~/token",
            "cmd:cat '/elsewhere/jira-1'",
            "keychain:jira-1",
            "env:JIRA_API_TOKEN",
            &s.replace("jira-1", ".jira-1.tmp"),
            &s.replace("jira-1", ".."),
            &format!("{s}; rm x"),
        ] {
            assert_eq!(name_of(other), None, "{other}");
        }
    }

    /// The shell reads the quoted path back as the path itself, whatever it holds.
    #[test]
    fn a_quote_in_the_path_stays_one_word() {
        let quoted = |p: &str| format!("'{}'", p.replace('\'', r"'\''"));
        let p = "/home/o'brien/my tokens/$HOME/jira-1";
        let out = std::process::Command::new("sh")
            .args(["-c", &format!("printf %s {}", quoted(p))])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), p);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        /// Only a source of this folder's own making names a file, and the name is a valid one.
        #[test]
        fn name_of_names_only_valid_files(s in ".{0,80}") {
            if let Some(name) = name_of(&s) {
                prop_assert!(is_name(name));
                prop_assert_eq!(source(name), s.trim());
            }
        }
    }

    #[test]
    fn saves_and_replaces_through_cat() {
        store("jira-1", "ATATT-first").unwrap();
        let read = || {
            let cmd = source("jira-1");
            let out = std::process::Command::new("sh")
                .args(["-c", cmd.strip_prefix("cmd:").unwrap()])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        assert_eq!(read(), "ATATT-first\n");
        store("jira-1", "ATATT-second").unwrap();
        assert_eq!(read(), "ATATT-second\n");
        // Only the token, and no temporary file left.
        let names: Vec<_> =
            std::fs::read_dir(dir()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["jira-1"]);
    }

    #[test]
    fn refuses_what_cannot_be_a_token_or_a_name() {
        assert_eq!(store("jira-1", "").unwrap_err(), "Paste the token into the Token field");
        let e = store("jira-1", "two words").unwrap_err();
        assert!(!e.contains("two words"), "never repeats the token: {e}");
        assert_eq!(store("../x", "ATATT-mock").unwrap_err(), "../x can't name a token file");
        assert_eq!(store(".x", "ATATT-mock").unwrap_err(), ".x can't name a token file");
        assert!(!dir().exists(), "nothing made");
    }

    /// Made private, not made private after: whatever the umask, nothing is ever open to others.
    #[test]
    fn folder_and_file_are_private_from_the_start() {
        // SAFETY: umask only sets the process's mask; it reads or writes no memory.
        let before = unsafe { libc::umask(0) };
        let stored = store("jira-1", "ATATT-mock");
        // SAFETY: as above, putting the mask back.
        unsafe { libc::umask(before) };
        stored.unwrap();
        assert_eq!(mode(&dir()), 0o700);
        assert_eq!(mode(&path("jira-1")), 0o600);
    }

    #[test]
    fn refuses_a_folder_others_can_open_and_says_how_to_fix_it() {
        std::fs::create_dir_all(dir()).unwrap();
        std::fs::set_permissions(dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let shown = crate::store::tilde(&dir());
        assert_eq!(
            store("jira-1", "ATATT-mock").unwrap_err(),
            format!("other users can open {shown}: run chmod 700 {shown}")
        );
        assert!(!path("jira-1").exists());
        std::fs::set_permissions(dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
        store("jira-1", "ATATT-mock").unwrap();
    }

    #[test]
    fn never_writes_through_a_link() {
        std::fs::create_dir_all(dir()).unwrap();
        let elsewhere = crate::store::config_path().with_file_name("elsewhere");
        std::fs::write(&elsewhere, "kept").unwrap();
        // A link where the file goes: refused, and what it leads to is untouched.
        std::fs::set_permissions(dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, path("jira-1")).unwrap();
        let e = store("jira-1", "ATATT-mock").unwrap_err();
        assert!(e.ends_with("jira-1 is a link, not a file: delete it, then paste the token again"));
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "kept");
        // A folder where the file goes.
        std::fs::create_dir(path("jira-2")).unwrap();
        let e = store("jira-2", "ATATT-mock").unwrap_err();
        assert!(e.ends_with("jira-2 isn't a file: delete it, then paste the token again"), "{e}");
    }

    #[test]
    fn never_uses_a_linked_folder() {
        let real = crate::store::config_path().with_file_name("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&real, dir()).unwrap();
        let e = store("jira-1", "ATATT-mock").unwrap_err();
        assert!(
            e.ends_with("tokens is a link, not a folder: delete it, then paste the token again")
        );
        assert!(!real.join("jira-1").exists());
    }
}
