//! clap definitions and dispatch. Commands are thin: open a [`Ctx`],
//! resolve a [`Target`], call gh (or `issue` / `project` / `memory`), print.

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::beads;
use crate::config::{self, Config};
use crate::fields;
use crate::gh;
use crate::ids::{self, IssueRef};
use crate::import;
use crate::init::{self, InitOpts};
use crate::issue::{self, Issue, Scope};
use crate::memory::{self, RememberOutcome};
use crate::project::{self, Board};
use crate::ready::{self, RankOpts, SortKey};
use crate::render;
use crate::repo::{self, Repo};
use std::fmt::Write as _;

#[derive(Parser, Debug)]
#[command(
    name = "gbd",
    version = env!("GBD_VERSION"),
    about = "GitHub Beads: Beads workflow on GitHub Issues and Projects",
    long_about = "gbd stores work in GitHub Issues. gh is the transport. There is no local database."
)]
pub struct Cli {
    /// GitHub OWNER/REPO. If omitted: `.gbd.yml` `repo:`, then `gh repo view`, then git origin.
    #[arg(long, global = true, value_name = "OWNER/REPO")]
    pub repo: Option<String>,

    /// JSON for agents.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(clap::Args, Debug, Clone)]
pub struct CreateArgs {
    pub title: String,
    /// Epic, Feature, Bug, Task, or Chore
    #[arg(short = 't', long, default_value = "Task")]
    pub r#type: String,
    /// P0–P4 or 0–4 (org Priority field)
    #[arg(short = 'p', long)]
    pub priority: Option<String>,
    /// Parent issue (sub-issue hierarchy)
    #[arg(long)]
    pub parent: Option<String>,
    /// Comma-separated issues this one is blocked by
    #[arg(long, value_name = "N,N")]
    pub deps: Option<String>,
    /// Comma-separated issues this one blocks
    #[arg(long, value_name = "N,N")]
    pub blocking: Option<String>,
    #[arg(short, long, default_value = "")]
    pub body: String,
    /// Print only owner/repo#n
    #[arg(long)]
    pub quiet: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Check gh, write .gbd.yml, org fields, memory issue, board, agent skill
    Init {
        #[arg(long)]
        no_project: bool,
        #[arg(long)]
        no_skills: bool,
        #[arg(long)]
        no_memory: bool,
    },
    /// Health check with copy-paste fixes
    Doctor {
        #[arg(long)]
        no_skills: bool,
    },
    /// Confirm gbd (and gh, if present) can boot
    Ping,
    /// Print inferred owner/repo and board
    Where,
    /// File an issue (type, priority, parent, blocked-by in one shot)
    Create(CreateArgs),
    /// Alias of create that prints only repo#n
    Q(CreateArgs),
    /// One issue: fields, parent, children, blockers
    Show { id: String },
    /// Issues as a tree (children under parents)
    List {
        /// open, closed, or all
        #[arg(long, default_value = "open")]
        state: String,
        #[arg(long)]
        r#type: Option<String>,
        #[arg(long)]
        assignee: Option<String>,
        /// Only sub-issues of this parent
        #[arg(long)]
        parent: Option<String>,
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
        /// Extra search qualifiers or text
        #[arg(long)]
        search: Option<String>,
        /// Flat rows instead of a tree
        #[arg(long)]
        flat: bool,
    },
    /// Search issues (GitHub search syntax)
    Search { query: String },
    /// Unblocked, unclaimed work, best first
    Ready {
        /// Claim the top item (assign @me, board → In Progress)
        #[arg(long)]
        claim: bool,
        #[arg(long)]
        explain: bool,
        /// Beads parity: a blocked parent hides its children
        #[arg(long)]
        strict_parent: bool,
        #[arg(long)]
        include_epics: bool,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[arg(long, default_value = "default")]
        sort: ReadySort,
    },
    /// Edit an issue; `--claim` assigns @me and moves it to In Progress
    Update {
        id: String,
        #[arg(long)]
        claim: bool,
        /// ready, `in_progress`, deferred, or done
        #[arg(long)]
        status: Option<String>,
        /// P0–P4 or 0–4
        #[arg(long)]
        priority: Option<String>,
        #[arg(long)]
        r#type: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        add_assignee: Option<String>,
        #[arg(long)]
        remove_assignee: Option<String>,
    },
    /// Assign someone (default @me)
    Assign {
        id: String,
        #[arg(default_value = "@me")]
        assignee: String,
    },
    /// Close an issue (board → Done)
    Close {
        id: String,
        /// completed, `not_planned`, or duplicate
        #[arg(long, default_value = "completed")]
        reason: String,
    },
    /// Reopen an issue (board → Ready)
    Reopen { id: String },
    /// Delete (prefer close --reason `not_planned`)
    Delete {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Dependencies (blocked-by / blocking)
    Dep {
        #[command(subcommand)]
        cmd: DepCmd,
    },
    /// Alias of dep add
    Link { issue: String, blocked_by: String },
    /// Sub-issues of a parent
    Children { id: String },
    /// Show or set the parent
    Parent {
        id: String,
        #[arg(long)]
        set: Option<String>,
        #[arg(long)]
        remove: bool,
    },
    /// Add a comment
    Comment { id: String, body: String },
    /// Alias of comment
    Note { id: String, body: String },
    /// List comments
    Comments { id: String },
    /// Add or remove labels (labels are not priority, type, or status)
    Label {
        id: String,
        #[arg(long)]
        add: Option<String>,
        #[arg(long)]
        remove: Option<String>,
    },
    /// Set Priority (P0–P4 or 0–4)
    Priority { id: String, value: String },
    /// Open issues that are is:blocked, with their blockers
    Blocked,
    /// Move a Beads tracker onto GitHub (one shot; --dry-run prints the plan)
    Import {
        /// JSONL from `bd export --include-memories`
        #[arg(long = "from-beads", value_name = "FILE")]
        from_beads: PathBuf,
        /// Print the plan and write nothing
        #[arg(long)]
        dry_run: bool,
        /// Create the issues. One shot, not a sync: run it once per tracker.
        #[arg(long)]
        yes: bool,
    },
    /// Store an insight (positional arg is CONTENT; key is derived)
    Remember {
        insight: String,
        #[arg(long)]
        key: Option<String>,
    },
    /// Read one memory
    Recall { key: String },
    /// Delete one memory
    Forget { key: String },
    /// List memories, optionally filtered
    Memories { search: Option<String> },
    /// Session start: workflow + memories + ready + assigned to me
    Prime {
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
    },
    /// Reprint the AGENTS.md snippet
    Onboard,
    /// Open the issue in a browser
    Open { id: String },
    /// Shell completions (bash, zsh, fish, …)
    Completion { shell: Shell },
    /// Count issues
    Count {
        #[arg(long, default_value = "open")]
        state: String,
    },
    /// One-shot counts: open, blocked, in progress, assigned to me
    Status,
    /// Print the status mapping
    Statuses,
    /// Print the issue types
    Types,
    /// The board: URL and items by Status; `board sync` adds open issues
    Board {
        #[command(subcommand)]
        cmd: Option<BoardCmd>,
    },
    /// Read or write .gbd.yml
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Alias of doctor
    Info,
    /// Close as duplicate of another issue
    Duplicate { id: String, of: String },
    /// Open issues not updated in N days
    Stale {
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
    /// Full help for every command
    Quickstart,
    /// How to upgrade
    Upgrade,
    /// Defer one or more issues (board → Deferred, Start date if --until)
    Defer {
        #[arg(required = true)]
        ids: Vec<String>,
        /// YYYY-MM-DD, today, tomorrow, +1h/+3d/+2w, next monday, or a weekday
        #[arg(long, value_name = "WHEN")]
        until: Option<String>,
        /// Why; recorded as a comment
        #[arg(long)]
        reason: Option<String>,
    },
    /// Undefer: clear Start date (board → Ready)
    Undefer {
        #[arg(required = true)]
        ids: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum DepCmd {
    /// `gbd dep add 142 88` → #142 is blocked by #88
    Add {
        issue: String,
        blocked_by: String,
    },
    Remove {
        issue: String,
        blocked_by: String,
    },
    /// Direct blockers and blockees
    List {
        id: String,
    },
    /// Blocked-by chain, blocking chain, and sub-issue tree
    Tree {
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum BoardCmd {
    /// Put every open issue on the board (Status Ready if it has none)
    Sync,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    Get { key: Option<String> },
    Set { key: String, value: String },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ReadySort {
    Default,
    Priority,
    Unblocks,
    Path,
}

impl From<ReadySort> for SortKey {
    fn from(s: ReadySort) -> Self {
        match s {
            ReadySort::Default => SortKey::Default,
            ReadySort::Priority => SortKey::Priority,
            ReadySort::Unblocks => SortKey::Unblocks,
            ReadySort::Path => SortKey::Path,
        }
    }
}

// ---------------------------------------------------------------------------
// Context and targets

/// Everything a command needs after arg parsing: the repo, config, output mode.
struct Ctx {
    repo: Repo,
    cfg: Config,
    json: bool,
}

impl Ctx {
    fn open(explicit: Option<&str>, json: bool) -> Result<Self> {
        init::require_auth()?;
        // A malformed .gbd.yml must not silently become "no board".
        let cfg = config::load()?;
        let repo = repo::resolve(explicit.or(cfg.repo.as_deref()))?;
        Ok(Self { repo, cfg, json })
    }

    fn scope(&self) -> Scope<'_> {
        Scope {
            project: self.cfg.project_number(),
            owner: self.repo.owner(),
        }
    }

    /// The configured board, if any. Loading it is two `gh project` calls.
    fn board(&self) -> Result<Option<Board>> {
        match self.cfg.project_number() {
            Some(n) => Board::load(self.repo.owner(), n).map(Some),
            None => Ok(None),
        }
    }

    fn target(&self, id: &str) -> Result<Target> {
        let r = IssueRef::parse(id)?;
        let label = r.display(Some(&self.repo.name_with_owner));
        let repo = match r.repo {
            Some(other) if other != self.repo.name_with_owner => Repo {
                name_with_owner: other,
            },
            _ => self.repo.clone(),
        };
        Ok(Target {
            repo,
            number: r.number,
            label,
        })
    }

    fn emit<T: Serialize>(&self, value: &T, human: impl FnOnce() -> String) {
        emit_to(self.json, value, human);
    }

    fn search_prefix(&self) -> String {
        format!("repo:{} is:issue", self.repo.name_with_owner)
    }
}

/// JSON record or human text, on stdout.
fn emit_to<T: Serialize>(json: bool, value: &T, human: impl FnOnce() -> String) {
    if json {
        println!("{}", serde_json::to_string_pretty(value).unwrap());
    } else {
        let text = human();
        if text.is_empty() {
            return;
        }
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
    }
}

/// One issue, possibly in another repo under the same owner.
struct Target {
    repo: Repo,
    number: u64,
    /// `#12`, or `owner/repo#12` when it is not the current repo.
    label: String,
}

impl Target {
    /// `gh issue <verb> <n> -R <repo> <flags…>`
    fn gh_issue(&self, verb: &str, flags: &[&str]) -> Result<String> {
        let n = self.number.to_string();
        let mut args = vec!["issue", verb, &n, "-R", &self.repo.name_with_owner];
        args.extend_from_slice(flags);
        gh::run(&args)
    }

    fn edit(&self, flags: &[&str]) -> Result<String> {
        self.gh_issue("edit", flags)
    }

    fn fetch(&self, scope: Scope<'_>) -> Result<issue::Detail> {
        issue::fetch(&self.repo, self.number, scope)
    }

    fn set_board_status(&self, board: Option<&Board>, status: &str) -> Result<Option<String>> {
        let Some(board) = board else {
            return Ok(None);
        };
        let url = format!(
            "https://github.com/{}/issues/{}",
            self.repo.name_with_owner, self.number
        );
        board.set_status(&url, status)?;
        Ok(Some(status.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Dispatch

pub fn run() -> Result<u8> {
    dispatch(Cli::parse())
}

fn dispatch(cli: Cli) -> Result<u8> {
    let json = cli.json;
    let explicit = cli.repo.as_deref();
    match cli.command {
        Commands::Ping => Ok(ping(json)),
        Commands::Completion { shell } => {
            generate(shell, &mut Cli::command(), "gbd", &mut io::stdout());
            Ok(0)
        }
        Commands::Quickstart => {
            println!("{}", Cli::command().render_long_help());
            Ok(0)
        }
        Commands::Onboard => {
            print!("{}", init::AGENTS_BLOCK);
            Ok(0)
        }
        Commands::Upgrade => {
            println!("curl -fsSL https://raw.githubusercontent.com/aigency/gbd/main/scripts/install.sh | sh");
            println!("# or: cargo binstall gbd");
            Ok(0)
        }
        Commands::Statuses => Ok(statuses(json)),
        Commands::Types => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&init::ISSUE_TYPES).unwrap()
                );
            } else {
                println!("{}", init::ISSUE_TYPES.join(" "));
            }
            Ok(0)
        }
        Commands::Config { cmd } => cmd_config(cmd, json),
        Commands::Doctor { no_skills } => cmd_doctor(explicit, no_skills, json),
        Commands::Info => cmd_doctor(explicit, false, json),
        Commands::Init {
            no_project,
            no_skills,
            no_memory,
        } => cmd_init(
            explicit,
            &InitOpts {
                no_project,
                no_skills,
                no_memory,
            },
            json,
        ),
        Commands::Where => {
            let ctx = Ctx::open(explicit, json)?;
            ctx.emit(
                &json!({ "repo": ctx.repo.name_with_owner, "project": ctx.cfg.project_number() }),
                || match ctx.cfg.project_number() {
                    Some(p) => format!("{}  project #{p}", ctx.repo.name_with_owner),
                    None => ctx.repo.name_with_owner.clone(),
                },
            );
            Ok(0)
        }
        Commands::Create(args) => cmd_create(&Ctx::open(explicit, json)?, &args),
        Commands::Q(mut args) => {
            args.quiet = true;
            cmd_create(&Ctx::open(explicit, json)?, &args)
        }
        Commands::Show { id } => cmd_show(&Ctx::open(explicit, json)?, &id),
        Commands::List {
            state,
            r#type,
            assignee,
            parent,
            limit,
            search,
            flat,
        } => {
            let ctx = Ctx::open(explicit, json)?;
            let mut q = vec![ctx.search_prefix()];
            match state.as_str() {
                "open" => q.push("is:open".into()),
                "closed" => q.push("is:closed".into()),
                "all" => {}
                other => bail!("--state must be open, closed, or all (got {other})"),
            }
            if let Some(t) = r#type {
                q.push(format!("type:{t}"));
            }
            if let Some(a) = assignee {
                q.push(format!("assignee:{a}"));
            }
            if let Some(p) = parent {
                let t = ctx.target(&p)?;
                q.push(format!(
                    "parent-issue:{}#{}",
                    t.repo.name_with_owner, t.number
                ));
            }
            if let Some(s) = search {
                q.push(s);
            }
            cmd_list(&ctx, &q.join(" "), limit, flat)
        }
        Commands::Search { query } => {
            let ctx = Ctx::open(explicit, json)?;
            let q = format!("{} {query}", ctx.search_prefix());
            cmd_list(&ctx, &q, 50, false)
        }
        Commands::Stale { days } => {
            let ctx = Ctx::open(explicit, json)?;
            let cutoff = fields::days_ago(days);
            let q = format!("{} is:open updated:<{cutoff}", ctx.search_prefix());
            cmd_list(&ctx, &q, 50, true)
        }
        Commands::Blocked => cmd_blocked(&Ctx::open(explicit, json)?),
        Commands::Import {
            from_beads,
            dry_run,
            yes,
        } => cmd_import(explicit, json, &from_beads, dry_run, yes),
        Commands::Ready {
            claim,
            explain,
            strict_parent,
            include_epics,
            limit,
            sort,
        } => cmd_ready(
            &Ctx::open(explicit, json)?,
            &ReadyArgs {
                claim,
                explain,
                limit,
                rank: RankOpts {
                    strict_parent,
                    include_epics,
                    sort: sort.into(),
                },
            },
        ),
        Commands::Update {
            id,
            claim,
            status,
            priority,
            r#type,
            title,
            body,
            add_assignee,
            remove_assignee,
        } => cmd_update(
            &Ctx::open(explicit, json)?,
            &id,
            &UpdateArgs {
                claim,
                status,
                priority,
                r#type,
                title,
                body,
                add_assignee,
                remove_assignee,
            },
        ),
        Commands::Assign { id, assignee } => {
            let ctx = Ctx::open(explicit, json)?;
            let t = ctx.target(&id)?;
            t.edit(&["--add-assignee", &assignee])?;
            ctx.emit(&json!({ "number": t.number, "assignee": assignee }), || {
                format!("assigned {} to {assignee}", t.label)
            });
            Ok(0)
        }
        Commands::Close { id, reason } => cmd_close(&Ctx::open(explicit, json)?, &id, &reason),
        Commands::Reopen { id } => {
            let ctx = Ctx::open(explicit, json)?;
            let t = ctx.target(&id)?;
            let board = ctx.board()?;
            // Card first, then the issue; if the reopen fails, put the card back.
            let status = release_card(&ctx, &t, board.as_ref())?;
            if let Err(err) = t.gh_issue("reopen", &[]) {
                let _ = t.set_board_status(board.as_ref(), project::STATUS_DONE);
                return Err(err);
            }
            // Open again, it blocks again.
            let moved = reconcile_cards(&ctx, &t, board.as_ref());
            ctx.emit(
                &json!({ "number": t.number, "state": "open", "status": status, "moved": moves_json(&moved) }),
                || with_moves(format!("reopened {}", t.label), &moved),
            );
            Ok(0)
        }
        Commands::Delete { id, yes } => {
            let ctx = Ctx::open(explicit, json)?;
            let t = ctx.target(&id)?;
            if !yes {
                bail!("refusing to delete {} without --yes (prefer: gbd close {} --reason not_planned)", t.label, t.number);
            }
            // The issue is about to vanish: capture what it blocks first.
            let board = ctx.board()?;
            let held = board.as_ref().map(|_| t.fetch(ctx.scope())).transpose()?;
            t.gh_issue("delete", &["--yes"])?;
            let mut moved = Vec::new();
            if let (Some(board), Some(d)) = (&board, held) {
                // GitHub counted the deleted issue as a blocker only while it
                // was open; the copies fetched above still include it.
                let was_open = d.issue.is_open();
                let freed: Vec<Issue> = d
                    .blocking
                    .into_iter()
                    .map(|mut b| {
                        if was_open {
                            b.open_blockers = b.open_blockers.saturating_sub(1);
                        }
                        b
                    })
                    .collect();
                if let Err(err) = reconcile_each(board, &freed, &mut moved) {
                    eprintln!("warning: board not reconciled: {err:#}. Run: gbd board sync");
                }
            }
            ctx.emit(
                &json!({ "number": t.number, "deleted": true, "moved": moves_json(&moved) }),
                || with_moves(format!("deleted {}", t.label), &moved),
            );
            Ok(0)
        }
        Commands::Dep { cmd } => cmd_dep(&Ctx::open(explicit, json)?, cmd),
        Commands::Link { issue, blocked_by } => cmd_dep(
            &Ctx::open(explicit, json)?,
            DepCmd::Add { issue, blocked_by },
        ),
        Commands::Children { id } => {
            let ctx = Ctx::open(explicit, json)?;
            let d = ctx.target(&id)?.fetch(ctx.scope())?;
            ctx.emit(&d.children, || render::tree(&d.children));
            Ok(0)
        }
        Commands::Parent { id, set, remove } => {
            cmd_parent(&Ctx::open(explicit, json)?, &id, set.as_deref(), remove)
        }
        Commands::Comment { id, body } | Commands::Note { id, body } => {
            let ctx = Ctx::open(explicit, json)?;
            let t = ctx.target(&id)?;
            let url = t.gh_issue("comment", &["--body", &body])?;
            ctx.emit(&json!({ "number": t.number, "comment": url }), || {
                url.clone()
            });
            Ok(0)
        }
        Commands::Comments { id } => cmd_comments(&Ctx::open(explicit, json)?, &id),
        Commands::Label { id, add, remove } => {
            let ctx = Ctx::open(explicit, json)?;
            let t = ctx.target(&id)?;
            let mut flags: Vec<&str> = Vec::new();
            if let Some(a) = &add {
                flags.extend(["--add-label", a]);
            }
            if let Some(r) = &remove {
                flags.extend(["--remove-label", r]);
            }
            if flags.is_empty() {
                bail!("nothing to do: pass --add and/or --remove");
            }
            t.edit(&flags)?;
            ctx.emit(
                &json!({ "number": t.number, "add": add, "remove": remove }),
                || format!("labels updated on {}", t.label),
            );
            Ok(0)
        }
        Commands::Priority { id, value } => {
            let rank = fields::parse_rank(&value)?;
            let ctx = Ctx::open(explicit, json)?;
            let t = ctx.target(&id)?;
            let option = fields::set_priority(&t.repo, t.number, rank)?;
            ctx.emit(&json!({ "number": t.number, "priority": option }), || {
                format!("{} Priority {option}", t.label)
            });
            Ok(0)
        }
        Commands::Remember { insight, key } => {
            cmd_remember(&Ctx::open(explicit, json)?, &insight, key.as_deref())
        }
        Commands::Recall { key } => cmd_recall(&Ctx::open(explicit, json)?, &key),
        Commands::Forget { key } => cmd_forget(&Ctx::open(explicit, json)?, &key),
        Commands::Memories { search } => {
            cmd_memories(&Ctx::open(explicit, json)?, search.as_deref())
        }
        Commands::Prime { limit } => cmd_prime(&Ctx::open(explicit, json)?, limit),
        Commands::Open { id } => {
            let ctx = Ctx::open(explicit, json)?;
            ctx.target(&id)?.gh_issue("view", &["--web"])?;
            Ok(0)
        }
        Commands::Count { state } => {
            let ctx = Ctx::open(explicit, json)?;
            let q = match state.as_str() {
                "all" => ctx.search_prefix(),
                s => format!("{} is:{s}", ctx.search_prefix()),
            };
            let n = issue::count(&q)?;
            ctx.emit(&json!({ "state": state, "count": n }), || n.to_string());
            Ok(0)
        }
        Commands::Status => cmd_status(&Ctx::open(explicit, json)?),
        Commands::Board { cmd: None } => cmd_board(&Ctx::open(explicit, json)?),
        Commands::Board {
            cmd: Some(BoardCmd::Sync),
        } => cmd_board_sync(&Ctx::open(explicit, json)?),
        Commands::Duplicate { id, of } => {
            let ctx = Ctx::open(explicit, json)?;
            let t = ctx.target(&id)?;
            let of = ctx.target(&of)?;
            let board = ctx.board()?;
            t.gh_issue("close", &["--reason", "duplicate"])?;
            let _ = t.gh_issue(
                "comment",
                &["--body", &format!("Duplicate of {}", of.label)],
            );
            let status = card_done_or_reopen(&t, board.as_ref())?;
            let moved = reconcile_cards(&ctx, &t, board.as_ref());
            ctx.emit(
                &json!({ "number": t.number, "duplicate_of": of.number, "status": status, "moved": moves_json(&moved) }),
                || with_moves(format!("{} closed as duplicate of {}", t.label, of.label), &moved),
            );
            Ok(0)
        }
        Commands::Defer { ids, until, reason } => cmd_defer(
            &Ctx::open(explicit, json)?,
            &ids,
            until.as_deref(),
            reason.as_deref(),
        ),
        Commands::Undefer { ids } => cmd_undefer(&Ctx::open(explicit, json)?, &ids),
    }
}

// ---------------------------------------------------------------------------
// Commands that need more than a few lines

fn ping(json: bool) -> u8 {
    let version = gh::version();
    let gh_ok = version.is_some_and(|v| v >= gh::MIN_GH_VERSION);
    let auth = gh_ok && gh::auth_logged_in();
    let gh_text = match version {
        None => "missing (brew install gh)".to_string(),
        Some(v) if v < gh::MIN_GH_VERSION => format!(
            "{} is too old; needs {} or newer (brew upgrade gh)",
            gh::version_string(v),
            gh::version_string(gh::MIN_GH_VERSION)
        ),
        Some(v) => gh::version_string(v),
    };
    if json {
        println!(
            "{}",
            json!({
                "ok": true, "gbd": env!("GBD_VERSION"),
                "gh": version.map(gh::version_string), "gh_ok": gh_ok,
                "gh_min": gh::version_string(gh::MIN_GH_VERSION), "auth": auth,
            })
        );
    } else {
        println!("gbd {}", env!("GBD_VERSION"));
        println!("gh: {gh_text}");
        if gh_ok {
            println!(
                "auth: {}",
                if auth {
                    "logged in"
                } else {
                    "not logged in (gh auth login)"
                }
            );
        }
    }
    0
}

fn statuses(json: bool) -> u8 {
    let rows = [
        (
            "open",
            "issue open, board Ready (or unassigned without a board)",
        ),
        (
            "in_progress",
            "board In Progress (or assigned without a board)",
        ),
        (
            "blocked",
            "open with an open blocker (GitHub is:blocked); board Blocked, kept by gbd",
        ),
        ("deferred", "board Deferred, or Start date in the future"),
        ("closed", "issue closed; board Done"),
    ];
    if json {
        let map: BTreeMap<_, _> = rows.iter().copied().collect();
        println!("{}", serde_json::to_string_pretty(&map).unwrap());
    } else {
        for (k, v) in rows {
            println!("{k:<12} {v}");
        }
    }
    0
}

fn cmd_init(explicit: Option<&str>, opts: &InitOpts, json: bool) -> Result<u8> {
    let cfg = config::load()?;
    let repo = repo::resolve(explicit.or(cfg.repo.as_deref()))?;
    let root = std::env::current_dir()?;
    let log = init::init(&root, &repo, opts)?;
    if json {
        println!(
            "{}",
            json!({ "ok": true, "repo": repo.name_with_owner, "log": log })
        );
    } else {
        for line in log {
            println!("{line}");
        }
    }
    Ok(0)
}

fn cmd_doctor(explicit: Option<&str>, no_skills: bool, json: bool) -> Result<u8> {
    let root = std::env::current_dir()?;
    let (cfg, cfg_error) = match config::load() {
        Ok(cfg) => (cfg, None),
        Err(err) => (Config::default(), Some(format!("{err:#}"))),
    };
    let repo = repo::resolve(explicit.or(cfg.repo.as_deref())).ok();
    let report = init::doctor(&root, repo.as_ref(), &cfg, cfg_error.as_deref(), !no_skills);
    if json {
        let checks: Vec<_> = report
            .lines
            .iter()
            .map(|c| json!({ "name": c.name, "ok": c.ok, "warn": c.warn, "detail": c.detail }))
            .collect();
        println!("{}", json!({ "ok": report.ok, "checks": checks }));
    } else {
        for c in &report.lines {
            let mark = match (c.ok, c.warn) {
                (false, _) => "FAIL",
                (true, true) => "!",
                (true, false) => "ok",
            };
            println!("{mark:4}  {}  {}", c.name, c.detail);
        }
    }
    Ok(u8::from(!report.ok))
}

/// What one `gh issue create` carries: title, body, type, parent, and both
/// edge lists, so there is never a create-then-attach step. `create` and
/// `import` both go through here.
struct NewIssue<'a> {
    title: &'a str,
    body: &'a str,
    issue_type: &'a str,
    parent: Option<u64>,
    blocked_by: &'a [u64],
    blocking: &'a [u64],
}

/// `12,13` for flags and JSON; None when empty.
fn csv(numbers: &[u64]) -> Option<String> {
    (!numbers.is_empty()).then(|| {
        numbers
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    })
}

/// Returns the new issue's number and URL.
fn create_issue(ctx: &Ctx, new: &NewIssue<'_>) -> Result<(u64, String)> {
    let parent = new.parent.map(|p| p.to_string());
    let deps = csv(new.blocked_by);
    let blocking = csv(new.blocking);
    let mut gh_args: Vec<&str> = vec![
        "issue",
        "create",
        "-R",
        &ctx.repo.name_with_owner,
        "--title",
        new.title,
        "--body",
        new.body,
        "--type",
        new.issue_type,
    ];
    if let Some(p) = &parent {
        gh_args.extend(["--parent", p]);
    }
    if let Some(d) = &deps {
        gh_args.extend(["--blocked-by", d]);
    }
    if let Some(b) = &blocking {
        gh_args.extend(["--blocking", b]);
    }
    let url = gh::run(&gh_args).with_context(|| {
        format!(
            "creating issue (type {:?} must exist on org {}: gbd types)",
            new.issue_type,
            ctx.repo.owner()
        )
    })?;
    Ok((ids::number_from_url(&url)?, url))
}

fn cmd_create(ctx: &Ctx, args: &CreateArgs) -> Result<u8> {
    let rank = args
        .priority
        .as_deref()
        .map(fields::parse_rank)
        .transpose()?;
    let parent = args
        .parent
        .as_deref()
        .map(|p| ctx.target(p))
        .transpose()?
        .map(|t| t.number);
    let deps = args
        .deps
        .as_deref()
        .map(ids::parse_number_list)
        .transpose()?
        .unwrap_or_default();
    let blocking = args
        .blocking
        .as_deref()
        .map(ids::parse_number_list)
        .transpose()?
        .unwrap_or_default();
    let (number, url) = create_issue(
        ctx,
        &NewIssue {
            title: &args.title,
            body: &args.body,
            issue_type: &args.r#type,
            parent,
            blocked_by: &deps,
            blocking: &blocking,
        },
    )?;
    let t = ctx.target(&number.to_string())?;

    let mut warnings = Vec::new();
    let priority = match rank {
        Some(r) => match fields::set_priority(&t.repo, number, r) {
            Ok(opt) => Some(opt),
            Err(err) => {
                warnings.push(format!("Priority not set: {err:#}"));
                None
            }
        },
        None => None,
    };
    let mut moved = Vec::new();
    let status = match ctx.board() {
        Ok(None) => None,
        Ok(Some(board)) => {
            // Edges move cards: the new issue starts Blocked while GitHub
            // counts an open blocker (a closed one blocks nothing), and what
            // it blocks leaves Ready. One fetch, only when an edge was given.
            let detail = if deps.is_empty() && blocking.is_empty() {
                None
            } else {
                match t.fetch(ctx.scope()) {
                    Ok(d) => Some(d),
                    Err(err) => {
                        warnings.push(format!(
                            "could not read dependencies (gbd board sync places the cards): {err:#}"
                        ));
                        None
                    }
                }
            };
            let initial = match &detail {
                Some(d) => ready_or_blocked(&d.issue),
                None if !deps.is_empty() => project::STATUS_BLOCKED,
                None => project::STATUS_READY,
            };
            let status = match t.set_board_status(Some(&board), initial) {
                Ok(s) => s,
                Err(err) => {
                    warnings.push(format!("not added to board: {err:#}"));
                    None
                }
            };
            if let Some(d) = &detail {
                if let Err(err) = reconcile_each(&board, &d.blocking, &mut moved) {
                    warnings.push(format!(
                        "blocked cards not moved (gbd board sync places them): {err:#}"
                    ));
                }
            }
            status
        }
        Err(err) => {
            warnings.push(format!("board unavailable: {err:#}"));
            None
        }
    };
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    let id = format!("{}#{number}", ctx.repo.name_with_owner);
    ctx.emit(
        &json!({
            "number": number, "id": id, "url": url, "title": args.title,
            "type": args.r#type, "priority": priority, "status": status,
            "parent": parent.map(|p| p.to_string()), "blocked_by": csv(&deps), "blocking": csv(&blocking),
            "moved": moves_json(&moved),
        }),
        || {
            if args.quiet {
                return id.clone();
            }
            let mut extra = Vec::new();
            if let Some(p) = &priority {
                extra.push(format!("Priority {p}"));
            }
            if let Some(s) = &status {
                extra.push(format!("Status {s}"));
            }
            let extra = if extra.is_empty() {
                String::new()
            } else {
                format!("  ({})", extra.join(", "))
            };
            with_moves(format!("{id}  {url}{extra}"), &moved)
        },
    );
    Ok(0)
}

fn cmd_show(ctx: &Ctx, id: &str) -> Result<u8> {
    let d = ctx.target(id)?.fetch(ctx.scope())?;
    ctx.emit(&d, || render::show(&d));
    Ok(0)
}

fn cmd_list(ctx: &Ctx, query: &str, limit: usize, flat: bool) -> Result<u8> {
    let issues = issue::search(query, limit, ctx.scope())?;
    ctx.emit(&issues, || {
        if issues.is_empty() {
            return "no issues".into();
        }
        if flat {
            issues
                .iter()
                .map(render::line)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            render::tree(&issues)
        }
    });
    Ok(0)
}

/// Beads → GitHub, planned in memory first. Only the plan exists so far:
/// without `--dry-run` the command refuses, so nothing half-imports.
fn cmd_import(
    explicit: Option<&str>,
    json: bool,
    from_beads: &Path,
    dry_run: bool,
    yes: bool,
) -> Result<u8> {
    let export = beads::load(from_beads)?;
    let plan = import::plan(&export);
    let source = from_beads.display().to_string();
    if dry_run {
        // Local: no gh, no auth, no repo lookup.
        emit_to(json, &plan, || import::render(&plan, &source, 25));
        return Ok(0);
    }
    let ctx = Ctx::open(explicit, json)?;
    if !yes {
        bail!(
            "this creates {} issues in {} and writes {} memories. Add --yes, or --dry-run to see the plan first",
            plan.items.len(),
            ctx.repo.name_with_owner,
            plan.memories.len()
        );
    }
    import_run(&ctx, &plan)
}

/// Execute the plan top to bottom: one `gh issue create` per bead, its
/// edges pointing at issues made earlier in this run, then Priority, Start
/// date, assignee, comments, the close, and the card. The create is fatal
/// (resuming is the mapping file's job); the rest warn and move on, since
/// `board sync` and one edit repair them.
fn import_run(ctx: &Ctx, plan: &import::Plan) -> Result<u8> {
    // Without a board, In Progress and Deferred have nowhere to go and the
    // beads would land in `ready` as plain open issues.
    let Some(board) = ctx.board()? else {
        bail!(
            "gbd import needs a board so In Progress and Deferred survive the move (.gbd.yml project:). Run: gbd init"
        );
    };
    if !ctx.json {
        let diagnostics = import::render_diagnostics(plan);
        if !diagnostics.is_empty() {
            println!("{diagnostics}");
        }
    }
    let org = ctx.repo.owner();
    let priority = fields::priority_field(org)?;
    let start_date = plan
        .items
        .iter()
        .any(|i| i.start_date.is_some())
        .then(|| fields::start_date_field(org))
        .transpose()?;
    let total = plan.items.len();
    let mut numbers: BTreeMap<&str, u64> = BTreeMap::new();
    let mut created = Vec::new();
    let mut closed_ok = 0usize;
    let mut warnings: Vec<String> = Vec::new();
    for (n, item) in plan.items.iter().enumerate() {
        let edge = |bead: &str| {
            numbers
                .get(bead)
                .copied()
                .with_context(|| format!("{}: {bead} was not created before it", item.bead))
        };
        let parent = item.parent.as_deref().map(edge).transpose()?;
        let blocked_by: Vec<u64> = item
            .blocked_by
            .iter()
            .map(|b| edge(b))
            .collect::<Result<_>>()?;
        let (number, url) = create_issue(
            ctx,
            &NewIssue {
                title: &item.title,
                body: &item.body,
                issue_type: item.issue_type,
                parent,
                blocked_by: &blocked_by,
                blocking: &[],
            },
        )
        .with_context(|| format!("{}: stopped after {n} of {total}", item.bead))?;
        numbers.insert(&item.bead, number);
        let t = ctx.target(&number.to_string())?;
        let mut warn = |what: String| warnings.push(format!("{} (#{number}): {what}", item.bead));
        if let Err(err) = fields::option_name(item.priority)
            .and_then(|p| fields::write_value(&ctx.repo, number, priority.id, p))
        {
            warn(format!("Priority not set: {err:#}"));
        }
        if let (Some(d), Some(field)) = (&item.start_date, &start_date) {
            if let Err(err) = fields::write_value(&ctx.repo, number, field.id, d) {
                warn(format!("Start date not set: {err:#}"));
            }
        }
        if let Some(login) = &item.assignee {
            if let Err(err) = t.edit(&["--add-assignee", login]) {
                warn(format!("assignee {login} not set: {err:#}"));
            }
        }
        for body in &item.comments {
            let n = number.to_string();
            if let Err(err) = gh::run_stdin(
                &[
                    "issue",
                    "comment",
                    &n,
                    "-R",
                    &ctx.repo.name_with_owner,
                    "--body-file",
                    "-",
                ],
                body.as_bytes(),
            ) {
                warn(format!("comment not added: {err:#}"));
            }
        }
        // What actually happened, not what was planned: a close that failed
        // leaves the issue open, so the card is left alone rather than Done.
        let mut state = item.state.clone();
        let mut status = Some(item.status);
        if let import::State::Closed { reason } = item.state {
            if let Err(err) = t.gh_issue("close", &["--reason", reason.as_flag()]) {
                warn(format!(
                    "not closed: {err:#}. Close it by hand, then: gbd board sync"
                ));
                state = import::State::Open;
                status = None;
            }
        }
        if let Some(s) = status {
            if let Err(err) = board.set_status(&url, s) {
                warn(format!("card not set to {s}: {err:#}. Run: gbd board sync"));
                status = None;
            }
        }
        if matches!(state, import::State::Closed { .. }) {
            closed_ok += 1;
        }
        if !ctx.json {
            let closed = if matches!(state, import::State::Closed { .. }) {
                "  ✓"
            } else {
                ""
            };
            println!(
                "{:>5}/{total}  {} → #{number}  [{}] {}{closed}",
                n + 1,
                item.bead,
                item.issue_type,
                item.title
            );
        }
        created.push(json!({
            "bead": item.bead, "number": number, "url": url,
            "state": state, "status": status,
        }));
    }
    // Memories: key/value onto the memories issue, last write wins.
    let mut memories = json!(null);
    if !plan.memories.is_empty() {
        match memories_for_import(ctx, &plan.memories) {
            Ok((issue, added, updated)) => {
                memories = json!({ "issue": issue, "added": added, "updated": updated });
            }
            Err(err) => warnings.push(format!("memories not imported: {err:#}")),
        }
    }
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    let dropped = plan.skipped.len();
    ctx.emit(
        &json!({
            "created": created, "memories": memories, "warnings": warnings,
            "skipped": plan.skipped, "cycles": plan.cycles, "problems": plan.problems,
        }),
        || {
            let closed = closed_ok;
            let mem = match &memories {
                serde_json::Value::Null => String::new(),
                m => format!(
                    "; memories: {} new, {} updated on #{}",
                    m["added"], m["updated"], m["issue"]
                ),
            };
            format!(
                "\nimported {total} issues ({closed} closed){mem}; {} warning{}; {dropped} could not map (listed above)",
                warnings.len(),
                if warnings.len() == 1 { "" } else { "s" }
            )
        },
    );
    Ok(0)
}

/// Upsert every `_type: memory` line into the memories issue.
fn memories_for_import(ctx: &Ctx, records: &[beads::Memory]) -> Result<(u64, usize, usize)> {
    let (issue, mut map) = memories(ctx)?;
    let (mut added, mut updated) = (0, 0);
    for m in records {
        if map.insert(m.key.clone(), m.value.clone()).is_some() {
            updated += 1;
        } else {
            added += 1;
        }
    }
    memory::save(&ctx.repo, issue, &map)?;
    Ok((issue, added, updated))
}

fn cmd_blocked(ctx: &Ctx) -> Result<u8> {
    let q = format!("{} is:open is:blocked", ctx.search_prefix());
    let issues = issue::search(&q, 100, ctx.scope())?;
    ctx.emit(&issues, || {
        if issues.is_empty() {
            return "nothing blocked".into();
        }
        issues
            .iter()
            .map(|i| {
                let by = i
                    .blocked_by_open
                    .iter()
                    .map(|n| format!("#{n}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}  ← {by}", render::line(i))
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(0)
}

struct ReadyArgs {
    claim: bool,
    explain: bool,
    limit: usize,
    rank: RankOpts,
}

fn cmd_ready(ctx: &Ctx, a: &ReadyArgs) -> Result<u8> {
    let issues = issue::snapshot(&ctx.repo, ctx.scope())?;
    let mut items = ready::rank(&issues, &a.rank);
    items.truncate(a.limit);
    if a.claim {
        let Some(top) = items.first() else {
            bail!("no ready work to claim");
        };
        let t = ctx.target(&top.number.to_string())?;
        let status = claim_issue(ctx, &t)?;
        ctx.emit(&json!({ "claimed": top, "status": status }), || {
            format!("claimed #{}  {}", top.number, top.title)
        });
        return Ok(0);
    }
    let by_number: BTreeMap<u64, &Issue> = issues.iter().map(|i| (i.number, i)).collect();
    ctx.emit(&items, || {
        if items.is_empty() {
            return "no ready work".into();
        }
        items
            .iter()
            .map(|r| {
                let line = by_number
                    .get(&r.number)
                    .map_or_else(|| format!("#{} {}", r.number, r.title), |i| render::line(i));
                if a.explain {
                    format!("{line}\n    {}", r.explain)
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(0)
}

/// Optimistic claim: read assignees, assign @me if empty, re-read. Then
/// move the board item to In Progress. Returns the board status set, if any.
///
/// The board is loaded before anything is written, so a missing scope or
/// project fails with no side effects; if the card move itself fails, the
/// assignment is rolled back so the issue does not vanish from `ready`.
fn claim_issue(ctx: &Ctx, t: &Target) -> Result<Option<String>> {
    let board = ctx.board()?;
    let assignees = |t: &Target| -> Result<Vec<String>> {
        let v: serde_json::Value = gh::run_json(&[
            "issue",
            "view",
            &t.number.to_string(),
            "-R",
            &t.repo.name_with_owner,
            "--json",
            "assignees",
        ])?;
        Ok(v.get("assignees")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("login").and_then(|l| l.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    };
    let before = assignees(t)?;
    if !before.is_empty() {
        bail!(
            "{} is already assigned to {}; not claiming",
            t.label,
            before.join(", ")
        );
    }
    // Resolve the login before mutating so no later step can fail for a
    // reason unrelated to the issue.
    let me = me()?;
    t.edit(&["--add-assignee", "@me"])?;
    // From here on every failure removes the assignee again, so a failed
    // claim never hides the issue from `ready` or blocks the next attempt.
    let unclaim = |err: anyhow::Error| {
        let _ = t.edit(&["--remove-assignee", "@me"]);
        err.context(format!("{}: claim rolled back", t.label))
    };
    // Issues take several assignees, so two racing claims both "succeed"
    // at the edit. The claim holds only if the caller is the sole assignee.
    let after = match assignees(t) {
        Ok(a) => a,
        Err(err) => return Err(unclaim(err)),
    };
    let others: Vec<&String> = after.iter().filter(|a| **a != me).collect();
    if !others.is_empty() {
        let _ = t.edit(&["--remove-assignee", "@me"]);
        bail!(
            "{}: claim lost the race to {}; try the next ready item",
            t.label,
            others
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !after.contains(&me) {
        bail!("{}: claim did not stick (still unassigned)", t.label);
    }
    t.set_board_status(board.as_ref(), project::STATUS_IN_PROGRESS)
        .map_err(unclaim)
}

/// After a close: move the card to Done, and reopen the issue if that fails
/// so the record and the board never disagree.
fn card_done_or_reopen(t: &Target, board: Option<&Board>) -> Result<Option<String>> {
    match t.set_board_status(board, project::STATUS_DONE) {
        Ok(status) => Ok(status),
        Err(err) => {
            let _ = t.gh_issue("reopen", &[]);
            Err(err.context(format!(
                "{}: board update failed; the issue was reopened. Retry the close",
                t.label
            )))
        }
    }
}

/// `(issue number, new Status)` for each card a command moved between
/// Ready and Blocked.
type Moves = Vec<(u64, &'static str)>;

/// Ready, or Blocked while GitHub counts an open blocker.
fn ready_or_blocked(i: &Issue) -> &'static str {
    if i.directly_blocked() {
        project::STATUS_BLOCKED
    } else {
        project::STATUS_READY
    }
}

/// Back into the pool after a reopen, an undefer, or `--status ready`:
/// Ready, or Blocked while GitHub counts an open blocker. One fetch, only
/// with a board.
fn release_card(ctx: &Ctx, t: &Target, board: Option<&Board>) -> Result<Option<String>> {
    let Some(board) = board else {
        return Ok(None);
    };
    let d = t.fetch(ctx.scope())?;
    t.set_board_status(Some(board), ready_or_blocked(&d.issue))
}

/// Blocked ⇄ Ready for one card, from GitHub's open-blocker count. Only
/// those two statuses move: In Progress, Deferred, and Done were someone's
/// decision, and an issue that is not on the board is `board sync`'s job.
fn reconcile_card(board: &Board, i: &Issue) -> Result<Option<&'static str>> {
    let want = ready_or_blocked(i);
    let movable = i.is_open()
        && (i.has_status(project::STATUS_READY) || i.has_status(project::STATUS_BLOCKED));
    if !movable || i.has_status(want) {
        return Ok(None);
    }
    board.set_status(&i.url, want)?;
    Ok(Some(want))
}

/// After a dependency edit, a close, or a reopen: one fetch of `t`, then its
/// own card and the cards of the open issues it blocks follow the new
/// open-blocker counts. Returns `(number, new status)` per card moved. A
/// failure here is a warning, not an error: the change itself is already on
/// GitHub, and `gbd board sync` repairs the board.
fn reconcile_cards(ctx: &Ctx, t: &Target, board: Option<&Board>) -> Moves {
    let Some(board) = board else {
        return Vec::new();
    };
    let mut moved = Vec::new();
    let outcome = t.fetch(ctx.scope()).and_then(|d| {
        reconcile_each(
            board,
            std::iter::once(&d.issue).chain(&d.blocking),
            &mut moved,
        )
    });
    if let Err(err) = outcome {
        eprintln!("warning: board not reconciled: {err:#}. Run: gbd board sync");
    }
    moved
}

/// `reconcile_card` over `issues`, recording each move in `moved`. Stops at
/// the first error; the moves made so far stay recorded.
fn reconcile_each<'a>(
    board: &Board,
    issues: impl IntoIterator<Item = &'a Issue>,
    moved: &mut Moves,
) -> Result<()> {
    for i in issues {
        if let Some(status) = reconcile_card(board, i)? {
            moved.push((i.number, status));
        }
    }
    Ok(())
}

fn moves_json(moved: &[(u64, &str)]) -> Vec<Value> {
    moved
        .iter()
        .map(|(n, s)| json!({ "number": n, "status": s }))
        .collect()
}

/// `closed #12 (completed); #14 → Ready, #15 → Ready`
fn with_moves(mut line: String, moved: &[(u64, &str)]) -> String {
    if !moved.is_empty() {
        let list: Vec<String> = moved.iter().map(|(n, s)| format!("#{n} → {s}")).collect();
        let _ = write!(line, "; {}", list.join(", "));
    }
    line
}

struct UpdateArgs {
    claim: bool,
    status: Option<String>,
    priority: Option<String>,
    r#type: Option<String>,
    title: Option<String>,
    body: Option<String>,
    add_assignee: Option<String>,
    remove_assignee: Option<String>,
}

fn cmd_update(ctx: &Ctx, id: &str, a: &UpdateArgs) -> Result<u8> {
    let t = ctx.target(id)?;
    let rank = a.priority.as_deref().map(fields::parse_rank).transpose()?;
    let wanted = a.status.as_deref().map(project::status_for).transpose()?;
    if a.claim && wanted.is_some() {
        bail!("--claim already sets the status; drop --status");
    }
    let mut changed: Vec<String> = Vec::new();

    // Ownership first: if the claim is refused, nothing else is touched.
    let mut status = None;
    if a.claim {
        status = claim_issue(ctx, &t)?;
        changed.push("claimed".into());
    }

    let mut flags: Vec<&str> = Vec::new();
    if let Some(v) = &a.title {
        flags.extend(["--title", v]);
    }
    if let Some(v) = &a.body {
        flags.extend(["--body", v]);
    }
    if let Some(v) = &a.r#type {
        flags.extend(["--type", v]);
    }
    if let Some(v) = &a.add_assignee {
        flags.extend(["--add-assignee", v]);
    }
    if let Some(v) = &a.remove_assignee {
        flags.extend(["--remove-assignee", v]);
    }
    if !flags.is_empty() {
        t.edit(&flags)?;
        changed.push("fields".into());
    }
    let mut priority = None;
    if let Some(r) = rank {
        priority = Some(fields::set_priority(&t.repo, t.number, r)?);
        changed.push("priority".into());
    }
    let mut moved = Vec::new();
    if let Some(s) = wanted {
        (status, moved) = set_status(ctx, &t, s)?;
        changed.push(format!("status {}", status.as_deref().unwrap_or(s)));
    }
    if changed.is_empty() {
        bail!("nothing to update; see gbd update --help");
    }
    ctx.emit(
        &json!({ "number": t.number, "changed": changed, "priority": priority, "status": status, "moved": moves_json(&moved) }),
        || with_moves(format!("updated {} ({})", t.label, changed.join(", ")), &moved),
    );
    Ok(0)
}

/// Beads `bd defer`: Start date (when `--until` is given) and board Status
/// Deferred. Without a board, `--until` is required, since the date is then
/// the only thing `ready` can see. `--reason` becomes a comment.
fn cmd_defer(ctx: &Ctx, ids: &[String], until: Option<&str>, reason: Option<&str>) -> Result<u8> {
    let date = until.map(fields::parse_until).transpose()?;
    let board = ctx.board()?;
    if date.is_none() && board.is_none() {
        bail!("deferring needs --until <when> (no board is configured, so the date is the signal)");
    }
    let mut results = Vec::new();
    for id in ids {
        let t = ctx.target(id)?;
        if let Some(d) = &date {
            fields::set_start_date(&t.repo, t.number, d)?;
        }
        let status = t.set_board_status(board.as_ref(), project::STATUS_DEFERRED)?;
        if let Some(why) = reason {
            let note = match &date {
                Some(d) => format!("Deferred until {d}: {why}"),
                None => format!("Deferred: {why}"),
            };
            t.gh_issue("comment", &["--body", &note])?;
        }
        results.push(json!({
            "number": t.number, "deferred": true, "start_date": date, "status": status, "reason": reason,
        }));
    }
    ctx.emit(&results, || {
        ids.iter()
            .map(|id| match &date {
                Some(d) => format!("#{} deferred until {d}", id.trim_start_matches('#')),
                None => format!("#{} deferred", id.trim_start_matches('#')),
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(0)
}

fn cmd_undefer(ctx: &Ctx, ids: &[String]) -> Result<u8> {
    let board = ctx.board()?;
    let mut results = Vec::new();
    for id in ids {
        let t = ctx.target(id)?;
        fields::clear_field(&t.repo, t.number, fields::START_DATE_FIELD)?;
        let status = release_card(ctx, &t, board.as_ref())?;
        results.push(json!({ "number": t.number, "deferred": false, "status": status }));
    }
    ctx.emit(&results, || {
        ids.iter()
            .map(|id| format!("#{} undeferred", id.trim_start_matches('#')))
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(0)
}

/// Beads `set-state`. Done closes (and frees what the issue blocked). With
/// a board, the card moves and assignees are untouched (ownership is
/// `--claim`'s job). Without one, the assignee is the only in-progress
/// signal, so it is set or cleared.
fn set_status(ctx: &Ctx, t: &Target, status: &str) -> Result<(Option<String>, Moves)> {
    let board = ctx.board()?;
    match (status, board.is_none()) {
        (project::STATUS_DONE, _) => {
            t.gh_issue("close", &["--reason", "completed"])?;
            let card = t.set_board_status(board.as_ref(), status)?;
            return Ok((card, reconcile_cards(ctx, t, board.as_ref())));
        }
        (project::STATUS_IN_PROGRESS, true) => {
            t.edit(&["--add-assignee", "@me"])?;
        }
        (project::STATUS_READY, true) => {
            let _ = t.edit(&["--remove-assignee", "@me"]);
        }
        (project::STATUS_READY, false) => {
            return Ok((release_card(ctx, t, board.as_ref())?, Vec::new()));
        }
        (project::STATUS_DEFERRED, true) => {
            bail!("deferred needs a board (.gbd.yml project:) or a date: gbd defer {} --until tomorrow", t.number);
        }
        _ => {}
    }
    Ok((t.set_board_status(board.as_ref(), status)?, Vec::new()))
}

fn cmd_close(ctx: &Ctx, id: &str, reason: &str) -> Result<u8> {
    let t = ctx.target(id)?;
    let reason = match reason {
        "completed" | "complete" | "done" => "completed",
        "not planned" | "not-planned" | "not_planned" | "wontfix" => "not planned",
        "duplicate" => "duplicate",
        other => bail!("--reason must be completed, not_planned, or duplicate (got {other})"),
    };
    // Board first: a missing scope or deleted project fails before the close.
    let board = ctx.board()?;
    t.gh_issue("close", &["--reason", reason])?;
    let status = card_done_or_reopen(&t, board.as_ref())?;
    // Whatever this issue was blocking may be free now.
    let moved = reconcile_cards(ctx, &t, board.as_ref());
    ctx.emit(
        &json!({ "number": t.number, "state": "closed", "reason": reason, "status": status, "moved": moves_json(&moved) }),
        || with_moves(format!("closed {} ({reason})", t.label), &moved),
    );
    Ok(0)
}

fn cmd_dep(ctx: &Ctx, cmd: DepCmd) -> Result<u8> {
    match cmd {
        DepCmd::Add { issue, blocked_by } => {
            let t = ctx.target(&issue)?;
            let b = ctx.target(&blocked_by)?;
            let by = b.repo.name_with_owner.clone() + "#" + &b.number.to_string();
            let flag = if b.repo.name_with_owner == t.repo.name_with_owner {
                b.number.to_string()
            } else {
                format!(
                    "https://github.com/{}/issues/{}",
                    b.repo.name_with_owner, b.number
                )
            };
            // Board first: a missing scope fails before the edit.
            let board = ctx.board()?;
            t.edit(&["--add-blocked-by", &flag])?;
            let moved = reconcile_cards(ctx, &t, board.as_ref());
            ctx.emit(
                &json!({ "issue": t.number, "blocked_by": by, "moved": moves_json(&moved) }),
                || with_moves(format!("{} is blocked by {}", t.label, b.label), &moved),
            );
            Ok(0)
        }
        DepCmd::Remove { issue, blocked_by } => {
            let t = ctx.target(&issue)?;
            let b = ctx.target(&blocked_by)?;
            let flag = if b.repo.name_with_owner == t.repo.name_with_owner {
                b.number.to_string()
            } else {
                format!(
                    "https://github.com/{}/issues/{}",
                    b.repo.name_with_owner, b.number
                )
            };
            let board = ctx.board()?;
            t.edit(&["--remove-blocked-by", &flag])?;
            let moved = reconcile_cards(ctx, &t, board.as_ref());
            ctx.emit(
                &json!({ "issue": t.number, "removed_blocked_by": b.number, "moved": moves_json(&moved) }),
                || with_moves(format!("{} no longer blocked by {}", t.label, b.label), &moved),
            );
            Ok(0)
        }
        DepCmd::List { id } => {
            let d = ctx.target(&id)?.fetch(ctx.scope())?;
            ctx.emit(
                &json!({ "issue": d.issue, "blocked_by": d.blocked_by, "blocking": d.blocking }),
                || {
                    let mut out = render::line(&d.issue);
                    out.push('\n');
                    for b in &d.blocked_by {
                        let _ = writeln!(out, "  ← blocked by  {}", render::line(b));
                    }
                    for b in &d.blocking {
                        let _ = writeln!(out, "  → blocking    {}", render::line(b));
                    }
                    out
                },
            );
            Ok(0)
        }
        DepCmd::Tree { id } => cmd_dep_tree(ctx, &id),
    }
}

/// Walk the open snapshot in memory: upstream blockers, downstream
/// blockees, and the sub-issue subtree. One paged query, no N+1.
fn cmd_dep_tree(ctx: &Ctx, id: &str) -> Result<u8> {
    fn chain<'a>(
        start: &'a Issue,
        next: impl Fn(&Issue) -> Vec<u64> + Copy,
        by_number: &BTreeMap<u64, &'a Issue>,
        depth: usize,
        seen: &mut HashSet<u64>,
        out: &mut Vec<(usize, &'a Issue)>,
    ) {
        for n in next(start) {
            let Some(i) = by_number.get(&n) else { continue };
            out.push((depth, i));
            if depth < 8 && seen.insert(n) {
                chain(i, next, by_number, depth + 1, seen, out);
            }
        }
    }
    let t = ctx.target(id)?;
    let issues = issue::snapshot(&t.repo, ctx.scope())?;
    let by_number: BTreeMap<u64, &Issue> = issues.iter().map(|i| (i.number, i)).collect();
    let root = by_number.get(&t.number).copied().with_context(|| {
        format!(
            "{} is not an open issue in {}",
            t.label, t.repo.name_with_owner
        )
    })?;
    let mut upstream = Vec::new();
    chain(
        root,
        |i| i.blocked_by_open.clone(),
        &by_number,
        1,
        &mut HashSet::new(),
        &mut upstream,
    );
    let mut downstream = Vec::new();
    chain(
        root,
        |i| i.blocking_open.clone(),
        &by_number,
        1,
        &mut HashSet::new(),
        &mut downstream,
    );
    let subtree: Vec<Issue> = {
        let mut keep: Vec<Issue> = vec![root.clone()];
        let mut frontier = vec![root.number];
        while let Some(p) = frontier.pop() {
            for i in issues.iter().filter(|i| i.parent == Some(p)) {
                keep.push(i.clone());
                frontier.push(i.number);
            }
        }
        keep
    };
    let rows = |v: &[(usize, &Issue)]| -> Vec<serde_json::Value> {
        v.iter()
            .map(|(d, i)| json!({ "depth": d, "issue": i }))
            .collect()
    };
    ctx.emit(
        &json!({ "issue": root, "blocked_by": rows(&upstream), "blocking": rows(&downstream), "subtree": subtree }),
        || {
            let mut out = String::new();
            let _ = writeln!(out, "{}", render::line(root));
            if !upstream.is_empty() {
                out.push_str("\nBLOCKED BY (upstream)\n");
                for (d, i) in &upstream {
                    let _ = writeln!(out, "{}← {}", "  ".repeat(*d), render::line(i));
                }
            }
            if !downstream.is_empty() {
                out.push_str("\nBLOCKING (downstream)\n");
                for (d, i) in &downstream {
                    let _ = writeln!(out, "{}→ {}", "  ".repeat(*d), render::line(i));
                }
            }
            if subtree.len() > 1 {
                out.push_str("\nSUB-ISSUES\n");
                out.push_str(&render::tree(&subtree));
            }
            out
        },
    );
    Ok(0)
}

fn cmd_parent(ctx: &Ctx, id: &str, set: Option<&str>, remove: bool) -> Result<u8> {
    let t = ctx.target(id)?;
    if remove {
        t.edit(&["--remove-parent"])?;
        ctx.emit(&json!({ "number": t.number, "parent": null }), || {
            format!("{} has no parent", t.label)
        });
        return Ok(0);
    }
    if let Some(p) = set {
        let p = ctx.target(p)?;
        let flag = if p.repo.name_with_owner == t.repo.name_with_owner {
            p.number.to_string()
        } else {
            format!(
                "https://github.com/{}/issues/{}",
                p.repo.name_with_owner, p.number
            )
        };
        t.edit(&["--parent", &flag])?;
        ctx.emit(&json!({ "number": t.number, "parent": p.number }), || {
            format!("{} is now a sub-issue of {}", t.label, p.label)
        });
        return Ok(0);
    }
    let d = t.fetch(ctx.scope())?;
    ctx.emit(&d.parent_issue, || match &d.parent_issue {
        Some(p) => render::line(p),
        None => format!("{} has no parent", t.label),
    });
    Ok(0)
}

fn cmd_comments(ctx: &Ctx, id: &str) -> Result<u8> {
    let t = ctx.target(id)?;
    let raw: Vec<serde_json::Value> = gh::run_json(&[
        "api",
        &format!(
            "repos/{}/issues/{}/comments",
            t.repo.name_with_owner, t.number
        ),
    ])?;
    let comments: Vec<serde_json::Value> = raw
        .iter()
        .map(|c| {
            json!({
                "id": c.get("id"),
                "user": c.pointer("/user/login"),
                "created_at": c.get("created_at"),
                "body": c.get("body"),
            })
        })
        .collect();
    ctx.emit(&comments, || {
        if comments.is_empty() {
            return format!("no comments on {}", t.label);
        }
        comments
            .iter()
            .map(|c| {
                format!(
                    "@{} ({})\n{}\n",
                    c["user"].as_str().unwrap_or("?"),
                    c["created_at"]
                        .as_str()
                        .unwrap_or("")
                        .split('T')
                        .next()
                        .unwrap_or(""),
                    c["body"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(0)
}

fn memories(ctx: &Ctx) -> Result<(u64, BTreeMap<String, String>)> {
    let n = memory::locate(&ctx.repo, ctx.cfg.memory_issue)?;
    Ok((n, memory::load(&ctx.repo, n)?))
}

fn cmd_remember(ctx: &Ctx, insight: &str, key: Option<&str>) -> Result<u8> {
    let (n, mut map) = memories(ctx)?;
    let (store_key, value, action) = match key {
        Some(k) => {
            let action = if map.contains_key(k) {
                "updated"
            } else {
                "remembered"
            };
            (k.to_string(), insight.to_string(), action)
        }
        None => match memory::remember_without_key(insight, &map)? {
            RememberOutcome::Recalled { key, value } => {
                ctx.emit(
                    &json!({ "key": key, "value": value, "action": "recalled", "found": true }),
                    || {
                        eprintln!("(recalled {key:?} — a bare existing key READS. To overwrite: gbd remember --key {key} \"…\")");
                        value.clone()
                    },
                );
                return Ok(0);
            }
            RememberOutcome::Store { key, value } => (key, value, "remembered"),
        },
    };
    map.insert(store_key.clone(), value.clone());
    memory::save(&ctx.repo, n, &map)?;
    ctx.emit(
        &json!({ "key": store_key, "value": value, "action": action }),
        || format!("{action} [{store_key}]: {value}"),
    );
    Ok(0)
}

fn cmd_recall(ctx: &Ctx, key: &str) -> Result<u8> {
    let (_, map) = memories(ctx)?;
    if let Some(v) = map.get(key) {
        ctx.emit(&json!({ "key": key, "value": v, "found": true }), || {
            v.clone()
        });
        Ok(0)
    } else {
        ctx.emit(&json!({ "key": key, "value": "", "found": false }), || {
            eprintln!("No memory with key {key:?}");
            String::new()
        });
        Ok(1)
    }
}

fn cmd_forget(ctx: &Ctx, key: &str) -> Result<u8> {
    let (n, mut map) = memories(ctx)?;
    if let Some(v) = map.remove(key) {
        memory::save(&ctx.repo, n, &map)?;
        ctx.emit(&json!({ "key": key, "deleted": true }), || {
            format!("Forgot [{key}]: {v}")
        });
        Ok(0)
    } else {
        ctx.emit(&json!({ "key": key, "found": false }), || {
            eprintln!("No memory with key {key:?}");
            String::new()
        });
        Ok(1)
    }
}

fn cmd_memories(ctx: &Ctx, search: Option<&str>) -> Result<u8> {
    let (_, map) = memories(ctx)?;
    let needle = search.unwrap_or("").to_lowercase();
    let filtered: BTreeMap<_, _> = map
        .into_iter()
        .filter(|(k, v)| {
            needle.is_empty()
                || k.to_lowercase().contains(&needle)
                || v.to_lowercase().contains(&needle)
        })
        .collect();
    ctx.emit(&filtered, || {
        if filtered.is_empty() {
            return match search {
                Some(_) => format!("No memories matching {needle:?}"),
                None => "No memories stored. Use `gbd remember \"insight\"` to add one.".into(),
            };
        }
        let mut out = format!("Memories ({}):\n\n", filtered.len());
        for (k, v) in &filtered {
            let _ = write!(out, "  {k}\n  {v}\n\n");
        }
        out
    });
    Ok(0)
}

fn cmd_prime(ctx: &Ctx, limit: usize) -> Result<u8> {
    let issues = issue::snapshot(&ctx.repo, ctx.scope())?;
    let ready: Vec<_> = ready::rank(&issues, &RankOpts::default())
        .into_iter()
        .take(limit)
        .collect();
    let by_number: BTreeMap<u64, &Issue> = issues.iter().map(|i| (i.number, i)).collect();
    let memories = memories(ctx).map(|(_, m)| m).unwrap_or_default();
    let mine = issue::search(
        &format!("{} is:open assignee:@me", ctx.search_prefix()),
        20,
        ctx.scope(),
    )
    .unwrap_or_default();
    ctx.emit(
        &json!({
            "workflow": "gbd ready → gbd show <n> → gbd update <n> --claim → gbd close <n>",
            "memories": memories,
            "ready": ready,
            "assigned_to_me": mine,
        }),
        || {
            let mut out = String::from("# gbd workflow\n\n");
            out.push_str("gbd ready → gbd show <n> → gbd update <n> --claim → gbd close <n>\n");
            out.push_str("gbd create \"…\" -t Task -p 1 --parent 88 --deps 12\n");
            out.push_str("gbd remember \"insight\"\n\n");
            let _ = write!(out, "## Persistent Memories ({})\n\n", memories.len());
            out.push_str(
                "Stored via `gbd remember`. Update with `gbd remember --key <key> \"…\"`.\n\n",
            );
            for (k, v) in &memories {
                let _ = write!(out, "### {k}\n{v}\n\n");
            }
            out.push_str("## Ready\n\n");
            if ready.is_empty() {
                out.push_str("no ready work\n");
            }
            for r in &ready {
                if let Some(i) = by_number.get(&r.number) {
                    out.push_str(&render::line(i));
                    out.push('\n');
                }
            }
            out.push_str("\n## Assigned to me\n\n");
            if mine.is_empty() {
                out.push_str("nothing\n");
            } else {
                out.push_str(&render::tree(&mine));
            }
            out
        },
    );
    Ok(0)
}

fn me() -> Result<String> {
    gh::run(&["api", "user", "--jq", ".login"])
}

/// Counts from one open snapshot (board Status when configured, assignee
/// otherwise) plus one search for what closed in the last week.
fn cmd_status(ctx: &Ctx) -> Result<u8> {
    let issues = issue::snapshot(&ctx.repo, ctx.scope())?;
    let work: Vec<&Issue> = issues.iter().filter(|i| !i.memory).collect();
    let me = me().unwrap_or_default();
    let open = work.len();
    let blocked = work.iter().filter(|i| i.directly_blocked()).count();
    let in_progress = work.iter().filter(|i| i.in_progress()).count();
    let deferred = work.iter().filter(|i| i.is_deferred()).count();
    let mine = work
        .iter()
        .filter(|i| i.assignees.iter().any(|a| a == &me))
        .count();
    let ready = ready::rank(&issues, &RankOpts::default()).len();
    let closed_week = issue::count(&format!(
        "{} is:closed closed:>={}",
        ctx.search_prefix(),
        fields::days_ago(7)
    ))?;
    ctx.emit(
        &json!({
            "open": open, "ready": ready, "blocked": blocked, "in_progress": in_progress,
            "deferred": deferred, "assigned_to_me": mine, "closed_last_7d": closed_week,
        }),
        || {
            format!(
                "open {open}  ready {ready}  blocked {blocked}  in-progress {in_progress}  deferred {deferred}  assigned-to-me {mine}  closed-last-7d {closed_week}"
            )
        },
    );
    Ok(0)
}

/// The board's own item list (so Done shows), with open rows enriched from
/// the snapshot for priority, type, and blocked state.
fn cmd_board(ctx: &Ctx) -> Result<u8> {
    let Some(board) = ctx.board()? else {
        bail!("no board configured. Run `gbd init` (creates one) or `gbd config set project <number>`");
    };
    let items = board.items()?;
    let open = issue::snapshot(&ctx.repo, ctx.scope())?;
    let by_number: BTreeMap<u64, &Issue> = open.iter().map(|i| (i.number, i)).collect();
    let mut by_status: BTreeMap<String, Vec<&project::BoardItem>> = BTreeMap::new();
    for it in &items {
        by_status
            .entry(it.status.clone().unwrap_or_else(|| "(no status)".into()))
            .or_default()
            .push(it);
    }
    let order = [
        project::STATUS_READY,
        project::STATUS_IN_PROGRESS,
        project::STATUS_BLOCKED,
        project::STATUS_DEFERRED,
        project::STATUS_DONE,
    ];
    let mut keys: Vec<&String> = by_status.keys().collect();
    keys.sort_by_key(|k| {
        order
            .iter()
            .position(|o| o.eq_ignore_ascii_case(k))
            .unwrap_or(order.len())
    });
    let here = ctx.repo.name_with_owner.as_str();
    let row = |it: &project::BoardItem| -> String {
        let local = it.repo.as_deref() == Some(here);
        match (it.number, it.repo.as_deref()) {
            (Some(n), _) if local => match by_number.get(&n) {
                Some(i) => render::line(i),
                None => format!("✓ #{n} {}", it.title),
            },
            (Some(n), Some(repo)) => format!("· {repo}#{n} {}", it.title),
            (Some(n), None) => format!("· #{n} {}", it.title),
            (None, _) => format!("· {} (draft)", it.title),
        }
    };
    ctx.emit(
        &json!({
            "number": board.number, "title": board.title, "url": board.url,
            "statuses": board.status_options.iter().map(|o| &o.name).collect::<Vec<_>>(),
            "items": items,
        }),
        || {
            let mut out = format!("{}  #{}  {}\n", board.title, board.number, board.url);
            if items.is_empty() {
                out.push_str("\n(empty) — claim, close, or create an issue to add items\n");
            }
            for k in keys {
                let rows = &by_status[k];
                let _ = writeln!(out, "\n{k} ({})", rows.len());
                for it in rows {
                    out.push_str("  ");
                    out.push_str(&row(it));
                    out.push('\n');
                }
            }
            out
        },
    );
    Ok(0)
}

/// Put every open work issue on the board and make Ready ⇄ Blocked agree
/// with GitHub's open-blocker counts. Cards that are In Progress, Deferred,
/// or Done are left alone. New cards become In Progress if assigned (the
/// pre-board convention), else Blocked or Ready.
fn cmd_board_sync(ctx: &Ctx) -> Result<u8> {
    let Some(board) = ctx.board()? else {
        bail!("no board configured. Run `gbd init` (creates one) or `gbd config set project <number>`");
    };
    // An org project can hold several repos' issues; numbers collide.
    let on_board: BTreeMap<u64, Option<String>> = board
        .items()?
        .into_iter()
        .filter(|it| it.repo.as_deref() == Some(ctx.repo.name_with_owner.as_str()))
        .filter_map(|it| it.number.map(|n| (n, it.status)))
        .collect();
    let open = issue::snapshot(&ctx.repo, ctx.scope())?;
    let mut added: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut moved: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut unchanged = 0usize;
    for i in open.iter().filter(|i| !i.memory) {
        let current = on_board.get(&i.number).and_then(Option::as_deref);
        let want = match current {
            None if !i.assignees.is_empty() => project::STATUS_IN_PROGRESS,
            None => ready_or_blocked(i),
            Some(s)
                if s.eq_ignore_ascii_case(project::STATUS_READY)
                    || s.eq_ignore_ascii_case(project::STATUS_BLOCKED) =>
            {
                ready_or_blocked(i)
            }
            Some(_) => {
                unchanged += 1;
                continue;
            }
        };
        if current.is_some_and(|s| s.eq_ignore_ascii_case(want)) {
            unchanged += 1;
            continue;
        }
        board.set_status(&i.url, want)?;
        let bucket = if current.is_some() {
            &mut moved
        } else {
            &mut added
        };
        bucket.entry(want).or_default().push(i.number);
    }
    ctx.emit(
        &json!({ "board": board.number, "added": added, "moved": moved, "unchanged": unchanged }),
        || {
            let mut parts = Vec::new();
            if !added.is_empty() {
                parts.push(format!("added {}", group_text(&added, "as")));
            }
            if !moved.is_empty() {
                parts.push(format!("moved {}", group_text(&moved, "to")));
            }
            if parts.is_empty() {
                parts.push("nothing to do".into());
            }
            format!(
                "board #{}: {}; {unchanged} unchanged",
                board.number,
                parts.join("; ")
            )
        },
    );
    Ok(0)
}

/// `#5 #6 as Ready, #9 as Blocked`
fn group_text(groups: &BTreeMap<&str, Vec<u64>>, joiner: &str) -> String {
    groups
        .iter()
        .map(|(status, nums)| {
            let list: Vec<String> = nums.iter().map(|n| format!("#{n}")).collect();
            format!("{} {joiner} {status}", list.join(" "))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn cmd_config(cmd: ConfigCmd, json: bool) -> Result<u8> {
    match cmd {
        ConfigCmd::Get { key } => {
            let cfg = config::load()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&cfg).unwrap());
            } else if let Some(k) = key {
                match k.as_str() {
                    "repo" => println!("{}", cfg.repo.unwrap_or_default()),
                    "memory_issue" => println!("{}", cfg.memory_issue.unwrap_or(0)),
                    "project" => {
                        println!("{}", cfg.project.map(|p| p.to_string()).unwrap_or_default());
                    }
                    other => bail!("unknown key {other}"),
                }
            } else {
                print!("{}", cfg.render());
            }
            Ok(0)
        }
        ConfigCmd::Set { key, value } => {
            let path = config::find().unwrap_or_else(|| PathBuf::from(".gbd.yml"));
            let mut cfg = if path.exists() {
                Config::load_from(&path)?
            } else {
                Config::default()
            };
            match key.as_str() {
                "repo" => cfg.repo = Some(value.clone()),
                "memory_issue" => cfg.memory_issue = Some(value.parse()?),
                "project" => {
                    cfg.project = Some(
                        value
                            .parse()
                            .context("project must be the board's number")?,
                    );
                }
                other => bail!("unknown key {other}"),
            }
            cfg.save_to(&path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&cfg).unwrap());
            } else {
                println!("set {key}={value}");
            }
            Ok(0)
        }
    }
}
