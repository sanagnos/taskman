//! A session's log: a JSON line per finished turn, beside its status file, which `taskman hook`
//! appends at the CLI's Stop and the TUI shows (`prefix d`). A turn's line holds the CLI's last
//! message of the turn, the files the session edited and the commits it made. The edits and the
//! commit the turn started from build up in a scratch file of their own (`Pending`) as the hooks
//! fire, and go into the turn's line at its end.
//!
//! Each line says what it is (`"kind"`) and readers skip kinds they don't know, so other events
//! can join the file later without a reader from before them failing. Like the status record, a
//! newer hook may write it for an older TUI: add fields, never rename or remove them, and read
//! whatever a field holds without failing the line.
//!
//! The message is the CLI's own output, which may quote a tracker or a PR: untrusted text, which
//! the TUI draws through `untrusted` and nothing puts in a brief or in `taskman status`.

use crate::status_file::{lenient, lenient_value};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// The log beside the status file at `status` (`store::status_file`).
pub fn log_path(status: &Path) -> PathBuf {
    status.with_extension("events.jsonl")
}

/// The turn under way's scratch file (`Pending`) beside the status file at `status`.
pub fn pending_path(status: &Path) -> PathBuf {
    status.with_extension("turn.jsonl")
}

/// The turns a log keeps: the newest, once it holds twice as many (`append`).
pub const KEEP: usize = 50;

/// The most of a message a turn keeps, in characters; a longer one ends in `…`.
const MESSAGE_KEPT: usize = 4000;

/// The most edited files a turn names; it counts the rest (`Turn::files_left_out`).
const FILES_KEPT: usize = 200;

/// The most commits a turn names, newest first; it counts the rest (`Turn::commits_left_out`).
const COMMITS_KEPT: usize = 20;

/// A finished turn. Every field may be missing or hold something else, as an older or newer
/// release wrote it; it then reads as its default.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct Turn {
    /// The commits made during the turn, newest first: those from the commit the turn started on
    /// to the one it ended on that the default branch doesn't have (`hook::made_since`).
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<String>,
    /// How many more commits it made than `commits` names.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "is_zero")]
    pub commits_left_out: usize,
    /// The files the session changed, relative to its folder where they're in it: those its edit
    /// tools named, in the order it first edited them, then those its commits changed.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// How many more files it changed than `files` names.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "is_zero")]
    pub files_left_out: usize,
    /// The CLI's last message of the turn, cut to `MESSAGE_KEPT`. Untrusted text.
    #[serde(default, deserialize_with = "lenient")]
    pub message: String,
    /// The CLI's conversation id.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// When the turn ended, in seconds since the epoch.
    #[serde(default, deserialize_with = "lenient")]
    pub ts: i64,
    /// The line the overlay shows for it (`markdown::headline`), worked out once by the thread
    /// that reads the log, not with every frame. Not in the log.
    #[serde(skip)]
    pub headline: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Turn {
    /// A turn's line, its message and files cut to what a line keeps.
    pub fn new(message: &str, files: Vec<String>, commits: Vec<String>, ts: i64) -> Self {
        let message = match message.char_indices().nth(MESSAGE_KEPT) {
            Some((at, _)) => format!("{}…", &message[..at]),
            None => message.to_string(),
        };
        let (files_left_out, mut files) = (files.len().saturating_sub(FILES_KEPT), files);
        files.truncate(FILES_KEPT);
        let (commits_left_out, mut commits) = (commits.len().saturating_sub(COMMITS_KEPT), commits);
        commits.truncate(COMMITS_KEPT);
        let session_id = None;
        let headline = String::new();
        Turn { commits, commits_left_out, files, files_left_out, message, session_id, ts, headline }
    }

    /// How many commits the turn made.
    pub fn commit_count(&self) -> usize {
        self.commits.len() + self.commits_left_out
    }

    /// How many files the turn edited.
    pub fn file_count(&self) -> usize {
        self.files.len() + self.files_left_out
    }

    /// The line for the log, without its newline.
    fn line(&self) -> String {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Value::Object(o) = &mut v {
            o.insert("kind".into(), "turn".into());
        }
        v.to_string()
    }
}

/// The turns in a log's text, newest first and at most `KEEP`. A line that isn't JSON, as a line
/// cut short or two run together, or that's another kind of event, is skipped.
pub fn parse(text: &str) -> Vec<Turn> {
    let mut turns: Vec<Turn> = text
        .lines()
        .rev()
        .filter_map(|l| match serde_json::from_str::<Value>(l).ok()? {
            v @ Value::Object(_) if v["kind"] == "turn" => Some(lenient_value(v)),
            _ => None,
        })
        .collect();
    turns.truncate(KEEP);
    turns
}

/// The turns in the log at `path`, newest first; none when there's no log.
pub fn read(path: &Path) -> Vec<Turn> {
    std::fs::read_to_string(path).map(|t| parse(&t)).unwrap_or_default()
}

/// Adds `turn` to the end of the log at `path`. Once the log holds twice `KEEP` lines, it's
/// written again with the newest `KEEP`, so most turns only append.
pub fn append(path: &Path, turn: &Turn) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).read(true).open(path)?;
    // A line an earlier hook left without its end, when it was stopped part-way, gets one, so
    // this line isn't run into it.
    let len = file.metadata()?.len();
    let mut last = [0u8];
    let ended = len == 0
        || (std::os::unix::fs::FileExt::read_at(&file, &mut last, len - 1)? == 1
            && last[0] == b'\n');
    let line = format!("{}{}\n", if ended { "" } else { "\n" }, turn.line());
    file.write_all(line.as_bytes())?;
    drop(file);
    let text = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > 2 * KEEP {
        let kept = lines[lines.len() - KEEP..].join("\n") + "\n";
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, kept)?;
        std::fs::rename(tmp, path)?;
    }
    Ok(())
}

/// What the hooks noted during the turn under way: the commit it started on and the files edited
/// so far. A scratch file of JSON lines, each `{"head": …}` or `{"files": […]}`, which the turn's
/// end reads and writes again with only the commit it ended on, marked `"stop": true`. A turn that
/// ends without a Stop hook (an interrupt) leaves it, so its edits go into the next turn's line; a
/// prompt replaces one a Stop wrote, and a Stop with no prompt since (Claude going on when a Stop
/// hook of the user's blocks it) starts from it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pending {
    pub head: Option<String>,
    pub files: Vec<String>,
    /// Whether the head is the one a Stop noted, rather than a prompt's.
    pub after_stop: bool,
}

/// The scratch file stops growing at this size: a turn that edits this much is counted short.
const PENDING_MAX: u64 = 1 << 20;

/// Notes a line in the scratch file at `path`. Small lines, appended whole, so two hooks firing
/// at once don't mix theirs.
fn note(path: &Path, v: &Value) -> io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > PENDING_MAX) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(format!("{v}\n").as_bytes())
}

/// Notes the commit a turn starts on. A turn that started without ending keeps its own.
pub fn note_start(path: &Path, head: &str) -> io::Result<()> {
    let noted = read_pending(path);
    if noted.after_stop {
        std::fs::remove_file(path)?;
    } else if noted.head.is_some() {
        return Ok(());
    }
    note(path, &serde_json::json!({ "head": head }))
}

/// Notes the commit a turn ended on, in a scratch file of its own (the Stop hook removes the
/// turn's first).
pub fn note_stop(path: &Path, head: &str) -> io::Result<()> {
    note(path, &serde_json::json!({ "head": head, "stop": true }))
}

/// Notes files a tool call edited.
pub fn note_files(path: &Path, files: &[String]) -> io::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    note(path, &serde_json::json!({ "files": files }))
}

/// What the scratch file at `path` noted: the first commit, and each file once, in the order
/// first noted. Lines that don't read are skipped.
pub fn read_pending(path: &Path) -> Pending {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut p = Pending::default();
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if p.head.is_none() {
            p.head = v["head"].as_str().map(str::to_string);
            p.after_stop = p.head.is_some() && v["stop"] == true;
        }
        for f in v["files"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if !p.files.iter().any(|x| x == f) {
                p.files.push(f.to_string());
            }
        }
    }
    p
}

/// The files a tool call edits, relative to `cwd` where they're in it: Claude's Edit, Write,
/// MultiEdit and NotebookEdit name the file, and Codex's apply_patch (as its own tool or a
/// command its shell tools run) carries a patch that names each one.
pub fn edited(tool: &str, input: &Value, cwd: &Path) -> Vec<String> {
    let paths = match tool {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ["file_path", "notebook_path"]
            .iter()
            .find_map(|k| input[k].as_str())
            .map(|p| vec![p.to_string()])
            .unwrap_or_default(),
        "apply_patch" => strings(input).iter().flat_map(|s| patch_files(s)).collect(),
        // Codex's shell tools, when the command runs apply_patch: a patch a command only quotes
        // (a heredoc writing a test fixture) edits nothing, and Claude's Bash never applies one.
        "shell" | "local_shell" | "shell_command" | "exec_command" | "unified_exec"
        | "container.exec"
            if strings(input).iter().any(|s| runs_apply_patch(s)) =>
        {
            strings(input).iter().flat_map(|s| patch_files(s)).collect()
        }
        _ => Vec::new(),
    };
    paths
        .into_iter()
        .map(|p| match Path::new(&p).strip_prefix(cwd) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
            _ => p,
        })
        .collect()
}

/// Whether the command or word `s` runs apply_patch: `apply_patch` as a word of its own, or a
/// script that starts with it (`apply_patch <<'EOF'`).
fn runs_apply_patch(s: &str) -> bool {
    s.trim_start()
        .strip_prefix("apply_patch")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t', '\n', '<']))
}

/// Every string in `v`, nested ones too: Codex has given a patch as the tool's input, its
/// `command`, or a word of it.
fn strings(v: &Value) -> Vec<&str> {
    match v {
        Value::String(s) => vec![s],
        Value::Array(a) => a.iter().flat_map(strings).collect(),
        Value::Object(o) => o.values().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

/// The files an apply_patch patch adds, changes, deletes or moves to; none when `s` isn't one.
pub fn patch_files(s: &str) -> Vec<String> {
    if !s.contains("*** Begin Patch") {
        return Vec::new();
    }
    let marks = ["*** Add File: ", "*** Update File: ", "*** Delete File: ", "*** Move to: "];
    s.lines()
        .filter_map(|l| marks.iter().find_map(|m| l.strip_prefix(m)))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = crate::store::scratch::dir().join("events_log");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        path
    }

    fn turn(n: i64) -> Turn {
        Turn::new(&format!("turn {n}"), vec![], vec![], n)
    }

    #[test]
    fn appends_and_reads_newest_first() {
        let path = scratch("ENG-52.events.jsonl");
        assert!(read(&path).is_empty());
        let mut first = Turn::new("Fixed the lint", vec!["src/a.rs".into()], vec!["abc".into()], 1);
        first.session_id = Some("5f0c".into());
        append(&path, &first).unwrap();
        append(&path, &turn(2)).unwrap();
        assert_eq!(read(&path), [turn(2), first]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.lines().next().unwrap(),
            r#"{"commits":["abc"],"files":["src/a.rs"],"kind":"turn","message":"Fixed the lint","session_id":"5f0c","ts":1}"#
        );
        assert_eq!(text.lines().nth(1).unwrap(), r#"{"kind":"turn","message":"turn 2","ts":2}"#);
    }

    #[test]
    fn keeps_the_newest_turns() {
        let keep = i64::try_from(KEEP).unwrap();
        let path = scratch("capped.events.jsonl");
        for n in 0..=2 * keep {
            append(&path, &turn(n)).unwrap();
        }
        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        assert_eq!(lines, KEEP, "written again once past twice KEEP");
        let turns = read(&path);
        assert_eq!(turns.len(), KEEP);
        assert_eq!(turns[0].ts, 2 * keep);
        // Under twice KEEP lines, it only appends; reading still gives KEEP.
        for n in 0..keep {
            append(&path, &turn(1000 + n)).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2 * KEEP);
        assert_eq!(read(&path).len(), KEEP);
        assert_eq!(read(&path)[0].ts, 1000 + keep - 1);
    }

    #[test]
    fn a_line_cut_short_is_skipped_and_ended() {
        let path = scratch("cut.events.jsonl");
        std::fs::write(&path, format!("{}\n{{\"kind\":\"turn\",\"mess", turn(1).line())).unwrap();
        assert_eq!(read(&path), [turn(1)]);
        append(&path, &turn(3)).unwrap();
        assert_eq!(read(&path), [turn(3), turn(1)], "the new line isn't run into the cut one");
    }

    #[test]
    fn reads_what_other_releases_wrote() {
        let text = [
            // Another kind of event, from a newer release.
            r#"{"kind":"message","from":"lead","to":"ENG-52","text":"fix it"}"#,
            // Odd values read as defaults; unknown fields are ignored.
            r#"{"kind":"turn","ts":"soon","message":7,"files":"a","commits":[1],"pushed":true}"#,
            r#"{"ts":5,"message":"no kind"}"#,
            "[1]",
            "",
            r#"{"kind":"turn","ts":9,"message":"done","files":["a","b"],"files_left_out":3}"#,
        ]
        .join("\n");
        let turns = parse(&text);
        assert_eq!(turns.len(), 2);
        assert_eq!((turns[0].ts, turns[0].message.as_str(), turns[0].file_count()), (9, "done", 5));
        assert_eq!(turns[1], Turn::default());
    }

    #[test]
    fn cuts_long_messages_and_file_lists() {
        let t = Turn::new(
            &"é".repeat(MESSAGE_KEPT + 5),
            (0..250).map(|n| n.to_string()).collect(),
            vec![],
            1,
        );
        assert_eq!(t.message.chars().count(), MESSAGE_KEPT + 1);
        assert!(t.message.ends_with('…'));
        assert_eq!((t.files.len(), t.files_left_out, t.file_count()), (FILES_KEPT, 50, 250));
        let t = Turn::new("", vec![], (0..25).map(|n| n.to_string()).collect(), 1);
        assert_eq!((t.commits.len(), t.commits_left_out, t.commit_count()), (COMMITS_KEPT, 5, 25));
        assert_eq!(t.commits[0], "0", "the newest kept");
    }

    #[test]
    fn notes_a_turn_as_it_goes() {
        let path = scratch("ENG-52.turn.jsonl");
        assert_eq!(read_pending(&path), Pending::default());
        note_start(&path, "aaa").unwrap();
        note_files(&path, &["src/a.rs".into()]).unwrap();
        note_files(&path, &[]).unwrap();
        // A prompt after an interrupt keeps the commit the turn started on.
        note_start(&path, "bbb").unwrap();
        note_files(&path, &["src/b.rs".into(), "src/a.rs".into()]).unwrap();
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"fi").unwrap();
        assert_eq!(
            read_pending(&path),
            Pending {
                head: Some("aaa".into()),
                files: vec!["src/a.rs".into(), "src/b.rs".into()],
                after_stop: false,
            }
        );
        // What a Stop leaves: the commit it ended on, which the next prompt replaces.
        std::fs::remove_file(&path).unwrap();
        note_stop(&path, "ccc").unwrap();
        note_files(&path, &["src/c.rs".into()]).unwrap();
        let after = read_pending(&path);
        assert_eq!((after.head.as_deref(), after.after_stop), (Some("ccc"), true));
        assert_eq!(after.files, ["src/c.rs"]);
        note_start(&path, "ddd").unwrap();
        assert_eq!(
            read_pending(&path),
            Pending { head: Some("ddd".into()), files: vec![], after_stop: false }
        );
    }

    #[test]
    fn finds_the_files_a_tool_edits() {
        let cwd = Path::new("/src/acme");
        let edit = json!({ "file_path": "/src/acme/src/main.rs", "old_string": "a" });
        assert_eq!(edited("Edit", &edit, cwd), ["src/main.rs"]);
        assert_eq!(edited("Write", &json!({ "file_path": "/tmp/x" }), cwd), ["/tmp/x"]);
        let nb = json!({ "notebook_path": "/src/acme/a.ipynb" });
        assert_eq!(edited("NotebookEdit", &nb, cwd), ["a.ipynb"]);
        assert!(edited("Read", &edit, cwd).is_empty());
        assert!(edited("Bash", &json!({ "command": "cargo test" }), cwd).is_empty());
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-a\n+b\n*** Add File: /src/acme/new.rs\n+x\n*** Delete File: old.rs\n*** Update File: a.rs\n*** Move to: b.rs\n*** End Patch";
        let want = ["src/lib.rs", "new.rs", "old.rs", "a.rs", "b.rs"];
        assert_eq!(edited("apply_patch", &json!({ "input": patch }), cwd), want);
        assert_eq!(edited("apply_patch", &json!(patch), cwd), want);
        assert_eq!(edited("shell", &json!({ "command": ["apply_patch", patch] }), cwd), want);
        let script = format!("apply_patch <<'EOF'\n{patch}\nEOF");
        assert_eq!(edited("exec_command", &json!({ "cmd": ["bash", "-lc", script] }), cwd), want);
        // A patch a command only writes out, as a test's fixture, edits nothing.
        let fixture = format!("cat > tests/fixtures/a.patch <<'EOF'\n{patch}\nEOF");
        assert!(edited("Bash", &json!({ "command": fixture }), cwd).is_empty());
        assert!(edited("shell", &json!({ "command": ["bash", "-lc", fixture] }), cwd).is_empty());
        assert!(edited("shell", &json!({ "command": ["apply_patches.sh"] }), cwd).is_empty());
        // Patch lines without the patch's start are just text.
        assert!(patch_files("*** Update File: a.rs").is_empty());
    }

    fn text() -> impl Strategy<Value = String> {
        "\\PC{0,12}"
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// What's written reads back the same.
        #[test]
        fn reads_back_what_it_writes(
            message in text(),
            files in prop::collection::vec(text(), 0..4),
            commits in prop::collection::vec("[0-9a-f]{7,40}", 0..3),
            ts in any::<i64>(),
        ) {
            let t = Turn::new(&message, files, commits, ts);
            prop_assert_eq!(parse(&t.line()), vec![t]);
        }

        /// Other processes write the log, and a hook can stop part-way: whatever the text, it
        /// reads, and every line that's a turn gives one.
        #[test]
        fn reads_any_text(lines in prop::collection::vec(prop_oneof![
            "\\PC{0,30}",
            text().prop_map(|m| turn(1).line().replace("turn 1", &m)),
            (0usize..40).prop_map(|n| turn(2).line().chars().take(n).collect()),
        ], 0..8)) {
            let text = lines.join("\n");
            let whole = lines.iter().filter(|l| {
                serde_json::from_str::<Value>(l).is_ok_and(|v| v["kind"] == "turn")
            }).count();
            prop_assert_eq!(parse(&text).len(), whole.min(KEEP));
        }
    }
}
