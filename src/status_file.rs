//! The status record each session's hooks write (`taskman hook`) and everything else reads: the
//! TUI's poll, `taskman status`, `taskman resume` and the leftovers check at startup.
//!
//! Sessions run the installed binary's hook, which after a background update is newer than the TUI
//! reading its file, and a debug build often runs beside the installed one. So the record must stay
//! readable both ways: add fields, never rename or remove them, and read whatever a field holds
//! without failing the record. A change an older release can't read is breaking under the 0.x rule.

use crate::store::{self, Ticket};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::path::Path;

/// What the session's last hook said it's doing.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum HookState {
    Idle,
    Working,
    Needs,
    Done,
    Ended,
    /// A word this release doesn't know, from a newer one, or none: read as idle.
    #[default]
    #[serde(other)]
    Unknown,
}

impl HookState {
    /// The state `taskman hook <word>` reports.
    pub fn from_word(word: &str) -> Self {
        lenient_value(Value::from(word))
    }

    /// What the sidebar, the pane header and `taskman status` call the state.
    pub fn word(self) -> &'static str {
        match self {
            HookState::Working => "working",
            HookState::Needs => "needs you",
            HookState::Done => "your turn",
            HookState::Ended => "exited",
            HookState::Idle | HookState::Unknown => "idle",
        }
    }
}

/// What a session that hasn't started is called, beside `HookState::word`.
pub const NOT_STARTED: &str = "not started";

/// Why a "needs" session waits on the user.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Reason {
    /// A permission prompt, which one key answers, so the TUI may clear it as soon as that key is
    /// pressed (`app::poll::prompt_key`).
    Permission,
    /// Questions, a plan, or a prompt of a kind taskman doesn't know: only a hook clears it.
    #[serde(other)]
    Question,
}

/// A session's status file. Every field may be missing or hold something else, as an older or
/// newer release wrote it; it then reads as its default.
///
/// The fields are in alphabetical order, the order `serde_json::Value` wrote them in before this
/// struct, so the file is the same byte for byte.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct Record {
    /// What a permission prompt asks to run: the tool's command, path or input, as the CLI's
    /// PermissionRequest hook gave it, cut short. Untrusted text; written for "needs" only.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Which start of the session wrote it (`TASKMAN_LAUNCH`), so the TUI can tell a record of an
    /// earlier one written late (`app::TicketView::takes`).
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub launch: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub message: String,
    /// The session's `--name` (`TASKMAN_SESSION_NAME`), which `taskman status` shows agents;
    /// sessions an older taskman started have none.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Written for "needs" only; older hooks write none, which reads as a question.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    /// The CLI's conversation id, written as `null` when the hook wasn't told one.
    #[serde(default, deserialize_with = "lenient")]
    pub session_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub state: HookState,
    /// The tool a permission prompt asks to use.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// When the hook wrote it, in seconds since the epoch.
    #[serde(default, deserialize_with = "lenient")]
    pub ts: i64,
}

impl Record {
    /// Whether the session waits on a permission prompt, rather than on something only a hook
    /// clears.
    pub fn permission(&self) -> bool {
        self.reason == Some(Reason::Permission)
    }
}

impl std::fmt::Display for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&serde_json::to_string(self).map_err(|_| std::fmt::Error)?)
    }
}

/// A field as its type, or its default when it holds something else: a record with one odd field
/// still says what the rest do.
fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    Ok(lenient_value(Value::deserialize(d)?))
}

fn lenient_value<T: DeserializeOwned + Default>(v: Value) -> T {
    T::deserialize(v).unwrap_or_default()
}

/// A record from a status file's text. None when it isn't JSON, as while a file is half written by
/// something that doesn't rename it into place; JSON that isn't an object reads as an empty record.
pub fn parse(text: &str) -> Option<Record> {
    match serde_json::from_str(text).ok()? {
        // Through a `Value`, so a key given twice is the last one, not an error.
        v @ Value::Object(_) => Some(lenient_value(v)),
        _ => Some(Record::default()),
    }
}

/// The record at `path`, if there's one to read.
pub fn read(path: &Path) -> Option<Record> {
    parse(&std::fs::read_to_string(path).ok()?)
}

/// Writes `record` to `path` whole: to a file beside it, then renamed over it. The file is this
/// process's own, since two hooks of one session can fire at once and a shared name lets one
/// rename the other's half-written file.
pub fn write(path: &Path, record: &Record) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, record.to_string())?;
    std::fs::rename(tmp, path)
}

/// The names of the sessions of `tickets` that run now, as their hooks recorded them, which
/// `store::session_name` mustn't give another session. `running` says which run: a command asks
/// each one's owner file (`resume::runs`), as only a process holding it runs one. `dir` is the
/// status folder (`store::status_dir`). The TUI goes by its own panes instead
/// (`App::qualified`): a session it just started has a name before its first hook writes it.
pub fn running_names<'a>(
    dir: &Path,
    tickets: impl IntoIterator<Item = &'a Ticket>,
    running: impl Fn(&Ticket) -> bool,
) -> Vec<String> {
    tickets
        .into_iter()
        .filter_map(|t| {
            // The record first: asking whether a session runs can take a few milliseconds.
            let name = read(&store::status_file_in(dir, &t.repo, &t.key))?.name?;
            running(t).then_some(name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    /// The record as releases up to v0.1.10 wrote it: a `Value` built field by field.
    fn as_v0_1_10(r: &Record) -> String {
        let state = serde_json::to_value(r.state).unwrap();
        let mut v =
            json!({ "state": state, "session_id": r.session_id, "message": r.message, "ts": r.ts });
        if let Some(name) = &r.name {
            v["name"] = json!(name);
        }
        if let Some(reason) = r.reason {
            v["reason"] = serde_json::to_value(reason).unwrap();
        }
        if let Some(launch) = &r.launch {
            v["launch"] = json!(launch);
        }
        // Added since: fields an older release never wrote, so absent when none.
        for (field, value) in [("tool", &r.tool), ("input", &r.input)] {
            if let Some(value) = value {
                v[field] = json!(value);
            }
        }
        v.to_string()
    }

    #[test]
    fn writes_what_older_releases_wrote() {
        let r = Record {
            state: HookState::Needs,
            session_id: Some("5f0c-11".into()),
            message: "Run the build".into(),
            ts: 1760000000,
            reason: Some(Reason::Permission),
            launch: Some("4242-1".into()),
            name: Some("alpha/WP-1".into()),
            ..Record::default()
        };
        assert_eq!(
            r.to_string(),
            r#"{"launch":"4242-1","message":"Run the build","name":"alpha/WP-1","reason":"permission","session_id":"5f0c-11","state":"needs","ts":1760000000}"#
        );
        let bare = Record { state: HookState::Done, ts: 1, ..Record::default() };
        assert_eq!(bare.to_string(), r#"{"message":"","session_id":null,"state":"done","ts":1}"#);
    }

    #[test]
    fn reads_what_older_and_newer_releases_wrote() {
        // v0.1.8's hook, in a session it started: every field it writes.
        let r = parse(r#"{"launch":"4242-1","message":"Claude needs your permission to use Bash","name":"WP-1","reason":"permission","session_id":"5f0c-11","state":"needs","ts":1760000000}"#).unwrap();
        assert_eq!(
            r,
            Record {
                state: HookState::Needs,
                session_id: Some("5f0c-11".into()),
                message: "Claude needs your permission to use Bash".into(),
                ts: 1760000000,
                reason: Some(Reason::Permission),
                launch: Some("4242-1".into()),
                name: Some("WP-1".into()),
                ..Record::default()
            }
        );
        // v0.1.14 and later: what a permission prompt asks for.
        let r = parse(r#"{"input":"cargo test","message":"","session_id":null,"state":"needs","tool":"Bash","ts":1}"#).unwrap();
        assert_eq!((r.tool.as_deref(), r.input.as_deref()), (Some("Bash"), Some("cargo test")));
        assert_eq!(parse(&r.to_string()), Some(r));
        // Odd values read as none.
        let r = parse(r#"{"input":["a"],"tool":3,"state":"needs"}"#).unwrap();
        assert_eq!((r.tool, r.input), (None, None));
        // The first releases' fields only.
        let r = parse(r#"{"state":"ended","session_id":"s","message":"","ts":2}"#).unwrap();
        assert_eq!((r.state, r.reason, r.launch, r.name), (HookState::Ended, None, None, None));
        // A newer release's state, reason and fields; and odd values, which read as defaults.
        let r = parse(r#"{"state":"blocked","reason":"review","cwd":"/r","ts":"soon","session_id":7,"message":null}"#).unwrap();
        assert_eq!(
            r,
            Record {
                state: HookState::Unknown,
                reason: Some(Reason::Question),
                ..Record::default()
            }
        );
        assert_eq!(r.state.word(), "idle");
        assert!(!r.permission());
        assert_eq!(parse("[1]"), Some(Record::default()));
        assert_eq!(parse(r#"{"state":"done""#), None);
        assert_eq!(HookState::from_word("needs"), HookState::Needs);
        assert_eq!(HookState::from_word("nonsense"), HookState::Unknown);
    }

    #[test]
    fn writes_whole_files() {
        let dir = store::scratch::dir().join("status_file");
        let path = dir.join("a/WP-1.json");
        let r = Record { state: HookState::Working, ts: 3, ..Record::default() };
        write(&path, &r).unwrap();
        assert_eq!(read(&path), Some(r));
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "no temp file");
    }

    fn state() -> impl Strategy<Value = HookState> {
        prop::sample::select(vec![
            HookState::Idle,
            HookState::Working,
            HookState::Needs,
            HookState::Done,
            HookState::Ended,
        ])
    }

    fn record() -> impl Strategy<Value = Record> {
        let text = || "\\PC{0,12}";
        (
            state(),
            prop::option::of(text()),
            text(),
            any::<i64>(),
            prop::option::of(prop::sample::select(vec![Reason::Permission, Reason::Question])),
            prop::option::of(text()),
            prop::option::of(text()),
            prop::option::of(text()),
            prop::option::of(text()),
        )
            .prop_map(
                |(state, session_id, message, ts, reason, launch, name, tool, input)| Record {
                    state,
                    session_id,
                    message,
                    ts,
                    reason,
                    launch,
                    name,
                    tool,
                    input,
                },
            )
    }

    /// Anything a field might hold.
    fn value() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            any::<f64>().prop_map(Value::from),
            "\\PC{0,8}".prop_map(Value::from),
            prop::sample::select(vec!["idle", "working", "needs", "done", "ended", "permission"])
                .prop_map(Value::from),
            Just(json!([1, "a"])),
            Just(json!({ "state": "done" })),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// The bytes are the ones releases before this struct wrote, which those read back.
        #[test]
        fn writes_the_same_bytes_and_reads_them_back(r in record()) {
            prop_assert_eq!(r.to_string(), as_v0_1_10(&r));
            prop_assert_eq!(parse(&r.to_string()), Some(r));
        }

        /// Other processes write these files: any JSON object reads, each field as what it holds
        /// when that's its type and as its default otherwise.
        #[test]
        fn reads_any_object(fields in prop::collection::btree_map(
            prop::sample::select(vec!["state", "session_id", "message", "ts", "reason", "launch", "name", "tool", "input", "cwd"]),
            value(),
            0..8,
        )) {
            let v = Value::Object(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
            let r = parse(&v.to_string()).unwrap();
            let s = |k: &str| v[k].as_str().map(str::to_string);
            prop_assert_eq!(r.state, HookState::from_word(v["state"].as_str().unwrap_or("")));
            prop_assert_eq!(&r.session_id, &s("session_id"));
            prop_assert_eq!(&r.message, &s("message").unwrap_or_default());
            prop_assert_eq!(r.ts, v["ts"].as_i64().unwrap_or(0));
            prop_assert_eq!(r.permission(), v["reason"] == "permission");
            prop_assert_eq!(r.reason.is_some(), v["reason"].is_string());
            prop_assert_eq!(r.launch, s("launch"));
            prop_assert_eq!(r.name, s("name"));
        }

        #[test]
        fn takes_any_text(s in "\\PC{0,60}") {
            parse(&s);
        }
    }
}
