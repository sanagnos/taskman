//! Forking a session into a new project agent (`prefix f`, `taskman add --agent NAME --fork KEY`):
//! the agent starts with a copy of the session's conversation so far, in the same CLI, and the
//! session carries on with its own. The CLI is asked first what it can do (`Cli::probe_fork`), on
//! a thread, and the agent is added once it answers (`forked`).

use super::sync::Msg;
use super::ticket::Added;
use super::{App, Field, Form, FormKind, Mode};
use crate::coding_cli::ForkProbe;
use crate::store::{self, Fork};
use crate::{add, untrusted, util};
use std::path::{Path, PathBuf};

/// A fork asked for: by the user's key, or by a session's `taskman add` the user approved.
#[derive(Clone, Debug)]
pub(crate) struct ForkAsk {
    /// The project's main checkout.
    pub repo: PathBuf,
    /// The key of the session to fork.
    pub source: String,
    /// The new agent's name, as typed.
    pub name: String,
    pub role: Option<String>,
    /// Who asked, for the toast (`Request::from`); None for the user.
    pub from: Option<String>,
}

impl ForkAsk {
    /// The fork a `taskman add` request asks for, if it asks for one.
    pub(super) fn of(req: &add::Request) -> Option<Self> {
        let (name, role) = req.agent()?;
        Some(ForkAsk {
            repo: req.repo.clone(),
            source: req.fork()?.to_string(),
            name: name.to_string(),
            role: role.map(str::to_string),
            from: req.from.clone(),
        })
    }
}

impl App {
    /// Opens the form that forks the selected session into a new agent (`prefix f`).
    pub(super) fn open_fork_form(&mut self) {
        let Some(tv) = self.tickets.get(self.sel) else { return };
        let (repo, key, cli) = (tv.t.repo.clone(), tv.t.key.clone(), tv.t.cli);
        if let Err(e) = self.fork_source(&repo, &key) {
            self.warn(e);
            return;
        }
        let source = self.session_name(self.sel);
        let mut form = Form::new(
            FormKind::Fork(repo, key),
            "Fork",
            vec![
                Field::text("Name", "", "the new agent's name: letters, digits and dashes"),
                Field::text("Role", "", "optional: a standing instruction for its brief"),
            ],
        );
        form.note = Some(format!(
            "Fork of {source}, as of when the fork starts: a new agent in {} with a copy of its conversation. {source} carries on as it is.",
            cli.label()
        ));
        self.mode = Mode::Form(Box::new(form));
    }

    /// The Fork form's Enter.
    pub(super) fn submit_fork(
        &mut self,
        form: &Form,
        repo: &Path,
        key: &str,
    ) -> Result<(), String> {
        let role = Some(form.value("Role")).filter(|r| !r.is_empty());
        let ask = ForkAsk {
            repo: repo.to_path_buf(),
            source: key.to_string(),
            name: form.value("Name"),
            role,
            from: None,
        };
        self.fork_session(ask)?;
        self.mode = Mode::Normal;
        Ok(())
    }

    /// Starts forking: checks what can be checked now, then asks the CLI on a thread; `forked`
    /// adds the agent when it answers. Err, with nothing started, when it can't be forked.
    pub(super) fn fork_session(&mut self, ask: ForkAsk) -> Result<(), String> {
        let (_, i) = self.check_fork(&ask)?;
        let cli = self.tickets[i].t.cli;
        let probe = self.probe_fork.clone();
        let again = ask.clone();
        self.reporter().spawn(
            format!("asking {} about forking", cli.product()),
            move || Msg::Forked(ask, probe(cli)),
            move |e| Msg::Forked(again, Err(e)),
        );
        // Asking the CLI can take a few seconds.
        self.toast(format!("Forking {}…", self.session_name(i)));
        Ok(())
    }

    /// Whether a request for a fork can be taken as it comes in, before asking the user: a
    /// session asking for one that can't be is told so at once.
    pub(super) fn can_fork(&self, req: &add::Request) -> Result<(), String> {
        ForkAsk::of(req).map_or(Ok(()), |ask| self.check_fork(&ask).map(|_| ()))
    }

    /// The fork's project and source, by index; Err when the fork can't be made: the project or
    /// the source is gone, the source has no conversation yet, or the name is taken.
    fn check_fork(&self, ask: &ForkAsk) -> Result<(usize, usize), String> {
        let Some(p) = self.projects.iter().position(|p| p.repo == ask.repo) else {
            return Err(format!(
                "the project in {} is no longer in taskman",
                store::tilde(&ask.repo)
            ));
        };
        let i = self.fork_source(&ask.repo, &ask.source)?;
        let name = store::slug(&ask.name);
        if name.is_empty() {
            return Err("Use letters or digits in the name".into());
        }
        if self.tickets.iter().any(|tv| tv.t.is_agent(&ask.repo, &name)) {
            return Err(format!("✦ {name} is already open: fork into another name"));
        }
        // That name would resume the closed agent's own conversation instead.
        if self.projects[p].closed_agents.iter().any(|c| c.name == name) {
            return Err(format!(
                "✦ {name} was closed with a conversation of its own, which that name resumes: pick another name"
            ));
        }
        Ok((p, i))
    }

    /// The session to fork, by its project and key; Err when it's gone or has no conversation to
    /// copy yet.
    fn fork_source(&self, repo: &Path, key: &str) -> Result<usize, String> {
        let Some(i) = self.tickets.iter().position(|tv| tv.t.is(repo, key)) else {
            return Err(format!("{} isn't open in taskman", untrusted::line(key)));
        };
        let t = &self.tickets[i].t;
        if !t.session_id.as_deref().is_some_and(|sid| t.cli.has_conversation(sid)) {
            return Err(format!(
                "{} has no conversation to fork yet: wait for its first turn",
                t.display()
            ));
        }
        Ok(i)
    }

    /// As agent `i` starts: a fork whose source's conversation is gone (its transcript removed)
    /// can't copy it, so it starts as a new agent would, and says so.
    pub(super) fn fork_source_gone(&mut self, i: usize) {
        let t = &self.tickets[i].t;
        let own = t.session_id.as_deref().is_some_and(|sid| t.cli.has_conversation(sid));
        if own || t.fork.as_ref().is_none_or(|f| t.cli.has_conversation(&f.from)) {
            return;
        }
        let msg = format!("✦ {}'s source conversation is gone, so it starts fresh", t.display());
        self.tickets[i].t.fork = None;
        self.save();
        self.warn(msg);
    }

    /// The CLI answered what it can do for fork `ask`: adds the agent, its session starting as a
    /// fork of the source's conversation as it is when it starts, or says why it can't.
    pub(super) fn forked(&mut self, ask: &ForkAsk, probe: Result<ForkProbe, String>) {
        let name = store::slug(&ask.name);
        let (p, i) = match self.check_fork(ask) {
            Ok(found) => found,
            Err(e) => return self.error(format!("Couldn't fork ✦ {name}: {e}")),
        };
        let probe = match probe {
            Ok(probe) => probe,
            Err(e) => return self.error(format!("Couldn't fork ✦ {name}: {e}")),
        };
        let source = &self.tickets[i].t;
        let cli = source.cli;
        if let Some(e) = cli.cant_fork(probe.version.as_deref()) {
            return self.error(e);
        }
        let id = match cli.chooses_fork_id().then(util::uuid_v4).transpose() {
            Ok(id) => id,
            Err(e) => return self.error(format!("Couldn't fork ✦ {name}: no id for it: {e}")),
        };
        let in_worktree = source.cwd != source.repo;
        let fork = Fork {
            from: source.session_id.clone().unwrap_or_default(),
            source: self.session_name(i),
            worktree: in_worktree.then(|| source.cwd.clone()),
            branch: source.branch.clone().filter(|_| in_worktree),
            extra: Default::default(),
        };
        let source = fork.source.clone();
        // The user's own fork is selected, unless they've moved on from the source meanwhile.
        let focus = ask.from.is_none() && self.sel == i;
        match self.add_agent(p, &name, cli, ask.role.clone(), focus) {
            Ok(Added::New(j)) => {
                let tv = &mut self.tickets[j];
                tv.t.session_id = id;
                tv.t.fork = Some(fork);
                tv.t.prompt_snapshot_off = probe.snapshot_off;
                tv.start = true;
                self.save();
                self.toast(match &ask.from {
                    Some(from) => {
                        format!("{} forked {source} into ✦ {name}", untrusted::line(from))
                    }
                    None => format!(
                        "Forked {source} into ✦ {name}: it starts with its conversation so far"
                    ),
                });
            }
            Ok(Added::Open(_)) => {
                self.error(format!("Couldn't fork ✦ {name}: it's already open"));
            }
            Err(e) => self.error(format!("Couldn't fork ✦ {name}: {e}")),
        }
    }
}
