//! Every setting of `config.toml`: where it is, what kind of value it takes,
//! what holds when it isn't set, what it does, and what it's set to now. The
//! settings view, `roster config` and the commented file `roster config
//! --defaults` prints all come from `sections`, which names every field of
//! `Config` (adding one without describing it doesn't compile).
//!
//! Changing a setting edits `config.toml` in place: the one value, comments
//! and layout kept (`toml_edit`), the whole file checked before it's written,
//! and written through a symlink (a dotfiles link stays a link).

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use toml_edit::{Array, DocumentMut, InlineTable, Item, Key, Table, TableLike, Value};

use crate::agent;
use crate::config::{
    AgentConfig, Config, First, FitPreview, KeyList, Keys, Order, ProjectKeys, SessionKeys,
    ViewKeys,
};

impl crate::engine::Roster {
    /// Every setting, as in effect now.
    pub fn settings(&self) -> Vec<Section> {
        let shell = crate::launch::shell(&self.config, std::env::var("SHELL").ok().as_deref());
        let chats = crate::util::short_path(&self.chats_dir());
        let agents = agent::available(&self.config);
        let env = Env {
            shell: &shell,
            chats_dir: &chats,
            agents: &agents,
        };
        sections(&self.config, &self.config_text, &env)
    }

    /// Changes the setting at `dotted` (`agents.pi.title_model`) to `input`,
    /// as typed; None (or empty text) for its default, which takes it out of
    /// config.toml. A whole table (`agents.aider`) can only be taken out.
    /// The file is read fresh, the change made in it, the result checked
    /// like a config at startup, and only then written and used.
    pub fn change_setting(&mut self, dotted: &str, input: Option<&str>) -> Result<()> {
        let path: Vec<String> = dotted.split('.').map(String::from).collect();
        let sections = self.settings();
        let item = match input {
            None => None,
            Some(input) => {
                let kind = kind_at(&sections, &path)
                    .with_context(|| format!("there's no setting {dotted:?}"))?;
                match kind {
                    Kind::Text if input.trim().is_empty() => None,
                    kind => Some(parse_input(&kind, input).with_context(|| dotted.to_string())?),
                }
            }
        };
        if item.is_none() && kind_at(&sections, &path).is_none() && !is_table(&sections, &path) {
            bail!("there's no setting {dotted:?}");
        }
        let file = self.paths.config_file();
        let text = crate::config::read_text(&file)?;
        let changed = edit(&text, &path, item)?;
        let config = Config::parse(&changed).with_context(|| format!("{dotted}: not changed"))?;
        write_file(&file, &changed)?;
        self.use_config(config, changed)
            .with_context(|| format!("{dotted} is changed, but roster's tmux didn't take it"))
    }

    /// Adds an agent that runs `line` (`codex --full-auto`): named after its
    /// command (`npx @openai/codex` after the package it runs), the rest its
    /// arguments. Returns its name.
    pub fn add_agent(&mut self, line: &str) -> Result<String> {
        let words = split_words(line)?;
        let (command, args) = words.split_first().context("no command")?;
        let name = agent_name(command, args);
        if !agent_named(&name) {
            bail!("{name:?} can't name an agent: letters, digits, '-' and '_'");
        }
        if agent::available(&self.config).contains(&name) {
            bail!("there's an agent {name:?} already");
        }
        let file = self.paths.config_file();
        let text = crate::config::read_text(&file)?;
        let path = |key: &str| vec!["agents".to_string(), name.clone(), key.to_string()];
        let mut changed = edit(
            &text,
            &path("command"),
            Some(toml_edit::value(command.as_str())),
        )?;
        if !args.is_empty() {
            let args = Item::Value(Value::Array(args.iter().map(String::as_str).collect()));
            changed = edit(&changed, &path("args"), Some(args))?;
        }
        let config = Config::parse(&changed).context("not added")?;
        write_file(&file, &changed)?;
        self.use_config(config, changed)
            .context("added, but roster's tmux didn't take the settings")?;
        Ok(name)
    }

    /// Reads config.toml again if it changed (an editor, a `git pull`) and
    /// puts it in use. If it's invalid now, the settings in use stay (the
    /// error). A file just written may not be whole yet (an editor can empty
    /// it first): it's left for a later call.
    pub fn reload_config(&mut self) -> Result<Reloaded> {
        let settling = fs::metadata(self.paths.config_file())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < SETTLE);
        if settling {
            return Ok(Reloaded::Same);
        }
        self.reload_config_now()
    }

    /// `reload_config` once the file is whole (the editor has closed).
    pub fn reload_config_now(&mut self) -> Result<Reloaded> {
        let file = self.paths.config_file();
        let text = match crate::config::read_text(&file) {
            Ok(text) => text,
            Err(e) => {
                let why = format!("{e:#}");
                if self.config_refused.as_ref() == Some(&why) {
                    return Ok(Reloaded::Same);
                }
                self.config_refused = Some(why);
                return Err(e);
            }
        };
        if text == self.config_text {
            // Put back as it was: a text refused before is news again.
            self.config_refused = None;
            return Ok(Reloaded::Same);
        }
        if self.config_refused.as_ref() == Some(&text) {
            return Ok(Reloaded::Same);
        }
        match Config::parse(&text) {
            Ok(config) => {
                self.config_refused = None;
                Ok(Reloaded::InUse(self.use_config(config, text).err()))
            }
            Err(e) => {
                self.config_refused = Some(text);
                Err(e.context(format!("invalid config {}", file.display())))
            }
        }
    }

    /// Puts `config` in use, then in roster's tmux too (its back key, its
    /// line): an error is about that, `config` is in use.
    fn use_config(&mut self, config: Config, text: String) -> Result<()> {
        self.config = config;
        self.config_text = text;
        if self.tmux.write_config(
            &self.config.keys.detach,
            &self.paths.user_tmux_conf(),
            self.config.status_line(),
        )? {
            self.tmux.reload_config()?;
        }
        Ok(())
    }
}

/// How long config.toml goes unchanged before roster reads what changed.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(300);

/// What reading config.toml again found.
pub enum Reloaded {
    /// The settings in use.
    Same,
    /// Changed settings, now in use; with roster's tmux's error if it didn't
    /// take them.
    InUse(Option<anyhow::Error>),
}

/// Whether `name` can name an agent (it's written in config.toml as is).
fn agent_named(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
}

/// The name an agent run by `command` gets: the command's, or, for one that
/// runs a package (`npx @openai/codex@latest`), the package's.
fn agent_name(command: &str, args: &[String]) -> String {
    let base = |word: &str| {
        Path::new(word)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(word)
            .to_string()
    };
    let name = base(command);
    if matches!(name.as_str(), "npx" | "bunx" | "pnpx" | "uvx" | "pipx")
        && let Some(package) = args.iter().find(|a| !a.starts_with('-') && *a != "run")
    {
        // `@scope/name@version`: the name.
        let package = package.strip_prefix('@').unwrap_or(package);
        let package = package.split('@').next().unwrap_or(package);
        return base(package.rsplit('/').next().unwrap_or(package));
    }
    name
}

/// The kind of the setting at `path`: one in `sections`, or one of an agent
/// not configured yet (`agents.codex.command`).
fn kind_at(sections: &[Section], path: &[String]) -> Option<Kind> {
    let found = sections
        .iter()
        .flat_map(|s| &s.settings)
        .find(|s| s.path == path)
        .map(|s| s.kind.clone());
    // An agent shown has its settings shown.
    let shown = |name: &str| {
        sections
            .iter()
            .any(|s| s.agent.as_ref().is_some_and(|(n, _)| n == name))
    };
    found.or_else(|| match path {
        [agents, name, key] if agents == "agents" && agent_named(name) && !shown(name) => {
            match key.as_str() {
                "command" | "title_model" => Some(Kind::Text),
                "args" => Some(Kind::Words),
                "idle_after" => Some(Kind::Seconds),
                "auto_title" => Some(Kind::Bool),
                "fit_preview" => Some(Kind::Choice(vec!["auto".into(), "never".into()])),
                _ => None,
            }
        }
        _ => None,
    })
}

fn is_table(sections: &[Section], path: &[String]) -> bool {
    sections.iter().any(|s| s.table.as_deref() == Some(path))
}

/// What a setting takes.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Bool,
    /// One of these words.
    Choice(Vec<String>),
    /// Any text.
    Text,
    /// Words (an agent's arguments), written on one line like a shell's.
    Words,
    /// A whole number of seconds, at least 1.
    Seconds,
    /// TUI keys: one, several, or none.
    Keys,
    /// A tmux key (the back key while attached).
    TmuxKey,
}

#[derive(Clone, Debug)]
pub struct Setting {
    /// Where it is in config.toml: `["agents", "pi", "title_model"]`.
    pub path: Vec<String>,
    pub kind: Kind,
    /// The value in effect, as shown.
    pub value: String,
    /// Whether config.toml sets it; if not, the default holds.
    pub set: bool,
    /// The value in effect, as it's typed (for editing): `k Up`, `--model x`.
    pub input: String,
    /// For text whose default isn't a value (pi's own default model): what
    /// to write in its place in a template.
    pub placeholder: Option<String>,
    /// What it does.
    pub doc: String,
}

impl Setting {
    /// Its name: the last part of its path.
    pub fn name(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }

    /// `agents.pi.title_model`, as `roster config set` takes it.
    pub fn dotted(&self) -> String {
        self.path.join(".")
    }
}

#[derive(Clone, Debug)]
pub struct Section {
    pub title: String,
    /// For an agent's: its name, and a note on it ("not installed").
    pub agent: Option<(String, String)>,
    /// Where it is in config.toml (`agents.pi`), for a section that's a table.
    pub table: Option<Vec<String>>,
    pub settings: Vec<Setting>,
}

/// What the values that depend on more than the file are: the shell, the
/// chats' directory, and which agents can be started.
pub struct Env<'a> {
    pub shell: &'a str,
    pub chats_dir: &'a str,
    pub agents: &'a [String],
}

/// Every setting, as `config` (read from `text`) has it.
pub fn sections(config: &Config, text: &str, cx: &Env) -> Vec<Section> {
    let doc: DocumentMut = text.parse().unwrap_or_default();
    let set = |path: &[&str]| lookup(doc.as_table(), path).is_some();
    let path = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let make = |p: &[&str], kind: Kind, value: String, input: String, doc: &str| Setting {
        path: path(p),
        kind,
        value,
        set: set(p),
        input,
        placeholder: None,
        doc: doc.to_string(),
    };
    let text_setting = |p: &[&str], value: Option<&str>, default: &str, doc: &str| {
        let shown = value.unwrap_or(default).to_string();
        make(p, Kind::Text, shown, value.unwrap_or("").to_string(), doc)
    };
    let choice = |p: &[&str], options: &[&str], value: &str, doc: &str| {
        let options = options.iter().map(|o| o.to_string()).collect();
        make(p, Kind::Choice(options), value.into(), value.into(), doc)
    };
    let bool_setting = |p: &[&str], value: bool, doc: &str| {
        make(
            p,
            Kind::Bool,
            on_off(value).into(),
            on_off(value).into(),
            doc,
        )
    };
    let keys_setting = |p: &[&str], keys: &KeyList, doc: &str| {
        let shown = if keys.0.is_empty() {
            "none".to_string()
        } else {
            keys.0.join(", ")
        };
        make(p, Kind::Keys, shown, keys.0.join(" "), doc)
    };

    // Every field, so a new one doesn't go undescribed.
    let Config {
        shell,
        chats_dir,
        order,
        keys,
        agents: configured,
        fit_preview,
        status_line,
        default_agent,
    } = config;
    let fit = |f: &Option<FitPreview>| match f.unwrap_or(FitPreview::Auto) {
        FitPreview::Auto => "auto",
        FitPreview::Never => "never",
    };
    let mut agents: Vec<&str> = cx.agents.iter().map(String::as_str).collect();
    if !agents.contains(&config.default_agent()) {
        agents.insert(0, config.default_agent());
    }
    let general = Section {
        title: "General".into(),
        agent: None,
        table: None,
        settings: vec![
            choice(
                &["default_agent"],
                &agents,
                default_agent.as_deref().unwrap_or(agent::CLAUDE),
                "the agent new sessions get unless their project picks one",
            ),
            bool_setting(
                &["status_line"],
                status_line.unwrap_or(true),
                "while attached, a line at the top saying where you are",
            ),
            choice(
                &["fit_preview"],
                &["auto", "never"],
                fit(fit_preview),
                "agents drawing full screen are sized to the preview while you're not attached",
            ),
            text_setting(
                &["shell"],
                shell.as_deref(),
                cx.shell,
                "what sources the env files before an agent starts (default: $SHELL, if POSIX-like)",
            ),
            text_setting(
                &["chats_dir"],
                chats_dir.as_deref(),
                cx.chats_dir,
                "where chats, sessions without a project of their own, run",
            ),
        ],
    };

    let Order {
        needs_input,
        idle,
        stopped,
        working,
        archived,
    } = order;
    let first = |f: &First| match f {
        First::Oldest => "oldest",
        First::Newest => "newest",
    };
    let shelf = |name: &str, f: &First, doc: &str| {
        choice(&["order", name], &["oldest", "newest"], first(f), doc)
    };
    let shelves = Section {
        title: "Shelves: which comes first".into(),
        agent: None,
        table: Some(path(&["order"])),
        settings: vec![
            shelf(
                "needs_input",
                needs_input,
                "needs input, and idle with a turn to read",
            ),
            shelf("idle", idle, "idle"),
            shelf("stopped", stopped, "stopped"),
            shelf("working", working, "working"),
            shelf("archived", archived, "archived, by when"),
        ],
    };

    let mut sections = vec![general, shelves];
    for name in agents {
        let adapter = agent::adapter(name);
        let agent = configured.get(name).cloned().unwrap_or_default();
        let AgentConfig {
            command,
            args,
            idle_after,
            fit_preview: agent_fit,
            title_model,
            auto_title,
        } = &agent;
        let p = |key: &'static str| -> Vec<&str> { vec!["agents", name, key] };
        let mut settings = vec![
            text_setting(
                &p("command"),
                command.as_deref(),
                name,
                "the command that starts it (default: its name, found on PATH)",
            ),
            make(
                &p("args"),
                Kind::Words,
                if args.is_empty() {
                    "none".into()
                } else {
                    shell_words(args)
                },
                shell_words(args),
                "added to every start",
            ),
            choice(
                &p("fit_preview"),
                &["auto", "never"],
                fit(&agent_fit.or(*fit_preview)),
                "as fit_preview above, for this agent",
            ),
        ];
        if adapter.oneoff(&Config::default()).is_some() {
            let (default, placeholder) = match name {
                agent::CLAUDE => ("haiku".to_string(), None),
                _ => (
                    format!("{name}'s own default"),
                    Some("\"<model>\"".to_string()),
                ),
            };
            let mut s = text_setting(
                &p("title_model"),
                title_model.as_deref(),
                &default,
                "the model a generated title is made with",
            );
            s.placeholder = placeholder;
            settings.push(s);
        }
        if !adapter.names_its_task() && adapter.oneoff(&Config::default()).is_some() {
            settings.push(bool_setting(
                &p("auto_title"),
                auto_title.unwrap_or(true),
                "a title made after a conversation's first turn (it names no task itself)",
            ));
        }
        if !adapter.reports() {
            let secs = idle_after.unwrap_or(crate::model::QUIET_MS as u64 / 1000);
            settings.push(make(
                &p("idle_after"),
                Kind::Seconds,
                format!("{secs}s"),
                secs.to_string(),
                "seconds of a still screen that count as idle (it reports nothing)",
            ));
        }
        let installed = configured.contains_key(name) || crate::util::on_path(name);
        let note = match (installed, name == config.default_agent()) {
            (false, _) => "not installed",
            (true, true) => "the default agent",
            (true, false) => "",
        };
        sections.push(Section {
            title: format!("Agent {name}"),
            agent: Some((name.to_string(), note.to_string())),
            table: Some(path(&["agents", name])),
            settings,
        });
    }

    let Keys {
        detach,
        up,
        down,
        collapse,
        expand,
        confirm,
        search,
        quit,
        help,
        session,
        project,
        view,
    } = keys;
    let k = |name: &str, list: &KeyList, doc: &str| keys_setting(&["keys", name], list, doc);
    sections.push(Section {
        title: "Keys".into(),
        agent: None,
        table: Some(path(&["keys"])),
        settings: vec![
            make(
                &["keys", "detach"],
                Kind::TmuxKey,
                detach.clone(),
                detach.clone(),
                "back to roster while attached (a tmux key)",
            ),
            k("up", up, "up"),
            k("down", down, "down (the list wraps around)"),
            k("collapse", collapse, "fold"),
            k("expand", expand, "unfold"),
            k(
                "confirm",
                confirm,
                "attach, fold or unfold a heading, restore when archived",
            ),
            k("search", search, "filter the list"),
            k("quit", quit, "quit (in a view: back)"),
            k("help", help, "every key"),
        ],
    });
    let SessionKeys {
        menu,
        new,
        edit,
        start,
        stop,
        restart,
        archive,
        delete,
        toggle_read,
        retitle,
        adopt,
        chat,
    } = session;
    let s =
        |name: &str, list: &KeyList, doc: &str| keys_setting(&["keys", "session", name], list, doc);
    sections.push(Section {
        title: "Keys: the session menu".into(),
        agent: None,
        table: Some(path(&["keys", "session"])),
        settings: vec![
            s("menu", menu, "opens the menu; the keys below act in it"),
            s("new", new, "a new session"),
            s("chat", chat, "a new chat, in no project of its own"),
            s("edit", edit, "edit"),
            s("start", start, "start"),
            s("stop", stop, "stop"),
            s("restart", restart, "restart"),
            s("archive", archive, "archive, or restore an archived one"),
            s("delete", delete, "delete an archived one for good"),
            s("toggle_read", toggle_read, "read / unread"),
            s(
                "retitle",
                retitle,
                "a title generated from the recent prompts",
            ),
            s("adopt", adopt, "adopt a conversation started elsewhere"),
        ],
    });
    let ProjectKeys {
        menu,
        new,
        edit,
        remove,
        filter,
        list,
    } = project;
    let pk =
        |name: &str, list: &KeyList, doc: &str| keys_setting(&["keys", "project", name], list, doc);
    sections.push(Section {
        title: "Keys: the project menu".into(),
        agent: None,
        table: Some(path(&["keys", "project"])),
        settings: vec![
            pk("menu", menu, "opens the menu; the keys below act in it"),
            pk("new", new, "a new project"),
            pk("edit", edit, "edit"),
            pk("remove", remove, "remove"),
            pk("filter", filter, "show only this project's sessions"),
            pk("list", list, "the projects view"),
        ],
    });
    let ViewKeys {
        menu,
        group,
        projects,
        server,
        settings,
    } = view;
    let v =
        |name: &str, list: &KeyList, doc: &str| keys_setting(&["keys", "view", name], list, doc);
    sections.push(Section {
        title: "Keys: the view menu".into(),
        agent: None,
        table: Some(path(&["keys", "view"])),
        settings: vec![
            v("menu", menu, "opens the menu; the keys below act in it"),
            v("group", group, "group by project, or by shelf"),
            v("projects", projects, "the projects view"),
            v("server", server, "the tmux server and the database"),
            v("settings", settings, "every setting, to see and change"),
        ],
    });
    sections
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// `words` on one line, quoted where a shell would need it.
pub fn shell_words(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            if !w.is_empty()
                && w.chars()
                    .all(|c| c.is_alphanumeric() || "-_=.,/:@+".contains(c))
            {
                w.clone()
            } else {
                format!("'{}'", w.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A line split into words as a shell would: on spaces, with '…' and "…"
/// keeping theirs.
pub fn split_words(line: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if let Some(w) = word.take() {
                    words.push(w);
                }
            }
            '\'' | '"' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some(q) if q == c => break,
                        Some('\\') if c == '"' => w.extend(chars.next()),
                        Some(other) => w.push(other),
                        None => bail!("a quote isn't closed"),
                    }
                }
            }
            '\\' => word.get_or_insert_with(String::new).extend(chars.next()),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    Ok(words)
}

/// The value `input` (as typed) is for a setting of `kind`.
pub fn parse_input(kind: &Kind, input: &str) -> Result<Item> {
    let input = input.trim();
    Ok(match kind {
        Kind::Bool => match input {
            "on" | "true" | "yes" => toml_edit::value(true),
            "off" | "false" | "no" => toml_edit::value(false),
            _ => bail!("{input:?} isn't on or off"),
        },
        Kind::Choice(options) => {
            if !options.iter().any(|o| o == input) {
                bail!("{input:?} isn't one of {}", options.join(", "));
            }
            toml_edit::value(input)
        }
        Kind::Text | Kind::TmuxKey => toml_edit::value(input),
        Kind::Seconds => {
            let secs: i64 = input
                .trim_end_matches('s')
                .parse()
                .with_context(|| format!("{input:?} isn't a number of seconds"))?;
            toml_edit::value(secs)
        }
        Kind::Words => Item::Value(Value::Array(split_words(input)?.into_iter().collect())),
        Kind::Keys => {
            let keys: Vec<&str> = input.split_whitespace().collect();
            match keys[..] {
                [] => toml_edit::value(""),
                [one] => toml_edit::value(one),
                _ => Item::Value(Value::Array(keys.into_iter().collect::<Array>())),
            }
        }
    })
}

/// `text` with the setting at `path` set to `item`, or, None, taken out (so
/// its default holds). Nothing else in it changes: not its other settings,
/// not its comments, not its layout.
pub fn edit(text: &str, path: &[String], item: Option<Item>) -> Result<String> {
    let (key, tables) = path.split_last().context("no setting")?;
    let changed = match item {
        Some(item) => set(text, tables, key, item)?,
        None => remove(text, path)?,
    };
    // Taking out the first setting can leave the blank line after it on top.
    Ok(changed.trim_start_matches('\n').to_string())
}

/// `text` with `key` of the table at `tables` (made if it isn't there) set to
/// `item`. A value it has already is replaced where it is, the comments
/// above it and after it kept.
fn set(text: &str, tables: &[String], key: &str, item: Item) -> Result<String> {
    let mut doc: DocumentMut = text.parse().context("config.toml isn't valid TOML")?;
    // A new table goes after the comments that end the file (all of them in
    // a file made from `template`), not above them.
    let trailer = doc
        .trailing()
        .as_str()
        .filter(|t| t.contains('#'))
        .map(String::from);
    let last = last_position(doc.as_table());
    let mut moved = false;
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    let mut inline = false;
    for (i, name) in tables.iter().enumerate() {
        let leaf = i + 1 == tables.len();
        let entry = table.entry(name).or_insert_with(|| {
            if inline {
                return Item::Value(Value::InlineTable(InlineTable::new()));
            }
            let mut t = Table::new();
            // `[agents.pi]`, not `[agents]` then `[agents.pi]`.
            t.set_implicit(!leaf);
            if leaf && let Some(trailer) = &trailer {
                t.set_position(Some(last + 1));
                t.decor_mut().set_prefix(format!("{trailer}\n"));
                moved = true;
            }
            Item::Table(t)
        });
        inline = entry.is_inline_table();
        table = entry
            .as_table_like_mut()
            .with_context(|| format!("{name} in config.toml isn't a table"))?;
    }
    match (table.get_mut(key).and_then(Item::as_value_mut), item) {
        (Some(old), Item::Value(mut new)) => {
            *new.decor_mut() = old.decor().clone();
            *old = new;
        }
        (_, item) => {
            table.insert(key, item);
        }
    }
    if moved {
        doc.set_trailing("");
    }
    Ok(doc.to_string())
}

/// The position of the table written last in `table`.
fn last_position(table: &Table) -> isize {
    table
        .iter()
        .filter_map(|(_, item)| item.as_table())
        .map(|t| t.position().unwrap_or(0).max(last_position(t)))
        .max()
        .unwrap_or(0)
}

/// `text` with the setting (or table) at `path` taken out: its lines, and
/// the comment just above them, but no comment further up (one a blank line
/// parts from it belongs to something else). A table it leaves empty goes
/// too, unless it's an agent's (which makes the agent) or keeps comments.
fn remove(text: &str, path: &[String]) -> Result<String> {
    let doc = toml_edit::Document::parse(text).context("config.toml isn't valid TOML")?;
    let (key, tables) = path.split_last().context("no setting")?;
    let mut table = doc.as_table();
    for name in tables {
        match table.get(name) {
            Some(Item::Table(t)) => table = t,
            // `pi = { args = [...] }`: the value taken out of the inline table.
            Some(Item::Value(Value::InlineTable(_))) => return remove_inline(text, path),
            _ => return Ok(text.to_string()),
        }
    }
    let (Some(k), Some(item)) = (table.key(key), table.get(key)) else {
        return Ok(text.to_string());
    };
    let mut cuts = Vec::new();
    lines_of(text, k, item, &mut cuts);
    let mut text = cut(text, cuts);
    // Up from the table it was in, those it leaves empty.
    for depth in (1..=tables.len()).rev() {
        let table = &tables[..depth];
        if table.len() == 2 && table[0] == "agents" {
            break;
        }
        match empty_table(&text, table)? {
            Some(cuts) => text = cut(&text, cuts),
            None => break,
        }
    }
    Ok(text)
}

/// The lines of the table at `path` in `text` if it has nothing in it (no
/// settings, no tables, no comments; a header of its own).
fn empty_table(text: &str, path: &[String]) -> Result<Option<Vec<Range<usize>>>> {
    let doc = toml_edit::Document::parse(text).context("config.toml isn't valid TOML")?;
    let (key, tables) = path.split_last().context("no table")?;
    let mut table = doc.as_table();
    for name in tables {
        match table.get(name) {
            Some(Item::Table(t)) => table = t,
            _ => return Ok(None),
        }
    }
    let (Some(k), Some(item @ Item::Table(t))) = (table.key(key), table.get(key)) else {
        return Ok(None);
    };
    if !t.is_empty() || t.is_implicit() || t.is_dotted() {
        return Ok(None);
    }
    let headers = headers(doc.as_table());
    let mut cuts = Vec::new();
    lines_of(text, k, item, &mut cuts);
    let Some(header) = t.span() else {
        return Ok(None);
    };
    // Its header and the comment above it go; comments under it would be
    // left under the table before it.
    let body = line_end(text, header.end)..region_end(text, &headers, header.start);
    let commented = text
        .get(body)
        .is_some_and(|b| b.lines().any(|l| l.trim_start().starts_with('#')));
    Ok((!commented).then_some(cuts))
}

/// Where each table header in `table` starts (its line), in order.
fn headers(table: &Table) -> Vec<usize> {
    fn visit(table: &Table, out: &mut Vec<usize>) {
        for (_, item) in table.iter() {
            if let Item::Table(t) = item {
                if !t.is_implicit()
                    && !t.is_dotted()
                    && let Some(span) = t.span()
                {
                    out.push(span.start);
                }
                visit(t, out);
            }
        }
    }
    let mut out = Vec::new();
    visit(table, &mut out);
    out.sort_unstable();
    out
}

/// Where what's under the header at `start` ends: at the next header (the
/// comment above it left for it), else at the end.
fn region_end(text: &str, headers: &[usize], start: usize) -> usize {
    headers
        .iter()
        .find(|&&h| h > start)
        .map_or(text.len(), |&h| comment_above(text, line_start(text, h)))
}

/// The parts of `text` that `key = item` takes: a value's lines; a table's
/// header and its own lines, up to its last value (comments after that are
/// left: they may be about anything); for each, the comment just above.
fn lines_of(text: &str, key: &Key, item: &Item, out: &mut Vec<Range<usize>>) {
    match item {
        Item::Value(v) => {
            if let (Some(k), Some(v)) = (key.span(), v.span()) {
                out.push(comment_above(text, line_start(text, k.start))..line_end(text, v.end));
            }
        }
        Item::Table(t) => {
            let header = (!t.is_implicit() && !t.is_dotted())
                .then(|| t.span())
                .flatten();
            if let Some(span) = &header {
                let end = t
                    .iter()
                    .filter_map(|(_, inner)| inner.as_value()?.span())
                    .map(|v| line_end(text, v.end))
                    .fold(line_end(text, span.end), usize::max);
                out.push(comment_above(text, line_start(text, span.start))..end);
            }
            for (name, inner) in t.iter() {
                // Under a header, its values are among its lines; its tables
                // have headers of their own, and a dotted one's values are
                // lines anywhere.
                if header.is_some() && !inner.is_table() {
                    continue;
                }
                if let Some(k) = t.key(name) {
                    lines_of(text, k, inner, out);
                }
            }
        }
        Item::ArrayOfTables(_) | Item::None => {}
    }
}

/// `text` without the parts `cuts` names, and without a blank line doubled
/// where they were.
fn cut(text: &str, mut cuts: Vec<Range<usize>>) -> String {
    cuts.sort_by_key(|c| c.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for c in cuts {
        match merged.last_mut() {
            Some(last) if c.start <= last.end => last.end = last.end.max(c.end),
            _ => merged.push(c),
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for c in merged {
        out.push_str(&text[at..c.start]);
        at = c.end;
        // A blank line before what was cut and one after it: one is enough.
        if (out.is_empty() || out.ends_with("\n\n")) && text[at..].starts_with('\n') {
            at += 1;
        }
    }
    out.push_str(&text[at..]);
    if out.ends_with("\n\n") {
        out.truncate(out.trim_end_matches('\n').len() + 1);
    }
    out
}

/// The start of the line `at` is on.
fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

/// The end of the line `at` is on (`at` itself if that's past a newline,
/// the end of a value at the end of its line), its newline included.
fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i + 1)
}

/// Where the comment just above the line that starts at `start` begins: the
/// lines above it that are comments, up to one that isn't, or that's a
/// setting or table commented out (`roster config --defaults` writes them).
fn comment_above(text: &str, mut start: usize) -> usize {
    while start > 0 {
        let above = line_start(text, start - 1);
        let line = &text[above..start];
        if !line.trim_start().starts_with('#') || commented_out(line) {
            break;
        }
        start = above;
    }
    start
}

/// Whether a comment is a setting or table commented out: `# up = "k"`,
/// `# [keys]`.
fn commented_out(line: &str) -> bool {
    let rest = line.trim_start().trim_start_matches('#').trim_start();
    let name = rest
        .find(|c: char| !(c.is_alphanumeric() || "_-.\"".contains(c)))
        .unwrap_or(rest.len());
    rest.starts_with('[') || (name > 0 && rest[name..].trim_start().starts_with('='))
}

/// `remove` for a setting in an inline table: toml_edit writes the table
/// again (an inline table holds no comments).
fn remove_inline(text: &str, path: &[String]) -> Result<String> {
    let mut doc: DocumentMut = text.parse().context("config.toml isn't valid TOML")?;
    let (key, tables) = path.split_last().context("no setting")?;
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for name in tables {
        match table.get_mut(name).and_then(Item::as_table_like_mut) {
            Some(inner) => table = inner,
            None => return Ok(text.to_string()),
        }
    }
    table.remove(key);
    Ok(doc.to_string())
}

fn lookup<'a>(table: &'a Table, path: &[&str]) -> Option<&'a Item> {
    let (key, tables) = path.split_last()?;
    let mut table: &dyn TableLike = table;
    for name in tables {
        table = table.get(name)?.as_table_like()?;
    }
    table.get(key)
}

/// Writes `text` to the config file at `path`: if it's a symlink (to
/// dotfiles, say), to the file it points to, so the link stays, also when
/// that file isn't there yet; whole, then moved into place, so nothing reads
/// half of it.
pub fn write_file(path: &Path, text: &str) -> Result<()> {
    let target = link_target(path);
    let dir = target.parent().context("config.toml has no directory")?;
    fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    let partial = target.with_extension(format!("toml.{}.partial", std::process::id()));
    let written = fs::write(&partial, text)
        .with_context(|| format!("can't write {}", partial.display()))
        .and_then(|()| {
            if let Ok(meta) = fs::metadata(&target) {
                fs::set_permissions(&partial, meta.permissions()).ok();
            }
            fs::rename(&partial, &target)
                .with_context(|| format!("can't write {}", target.display()))
        });
    if written.is_err() {
        fs::remove_file(&partial).ok();
    }
    written
}

/// The file a write to `path` should change: the one at the end of its
/// symlinks, whether it's there or not.
fn link_target(path: &Path) -> PathBuf {
    let mut path = path.to_path_buf();
    // A cycle of links ends somewhere; it fails at the write.
    for _ in 0..40 {
        match fs::read_link(&path) {
            Ok(to) => {
                path = match path.parent() {
                    Some(dir) => dir.join(to),
                    None => to,
                };
            }
            Err(_) => break,
        }
    }
    path
}

/// A config.toml with every setting commented out at what holds without it,
/// each with what it does: to start from, or compare with.
pub fn template(sections: &[Section]) -> String {
    let mut out = String::from(
        "# agent-roster's config.toml: every setting, at what holds when it's not set.\n# Uncomment and change what you want; roster's settings view (v s) does it too.\n",
    );
    for section in sections {
        out.push('\n');
        match &section.table {
            Some(table) => {
                out.push_str(&format!("# [{}]    # {}\n", table.join("."), section.title))
            }
            None => out.push_str(&format!("# {}\n", section.title)),
        }
        for s in &section.settings {
            let value = match parse_input(&s.kind, &s.input) {
                // Unset text: its default, if that's a value.
                Ok(_) if s.input.is_empty() && matches!(s.kind, Kind::Text) => s
                    .placeholder
                    .clone()
                    .unwrap_or_else(|| toml_string(&s.value)),
                Ok(item) => item.to_string().trim().to_string(),
                Err(_) => toml_string(&s.input),
            };
            out.push_str(&format!("# {} = {value}    # {}\n", s.name(), s.doc));
        }
    }
    out
}

fn toml_string(s: &str) -> String {
    toml_edit::value(s).to_string().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx() -> Env<'static> {
        static AGENTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        Env {
            shell: "/bin/zsh",
            chats_dir: "~/chats",
            agents: AGENTS.get_or_init(|| vec!["claude".into(), "pi".into(), "aider".into()]),
        }
    }

    fn find<'a>(sections: &'a [Section], dotted: &str) -> &'a Setting {
        sections
            .iter()
            .flat_map(|s| &s.settings)
            .find(|s| s.dotted() == dotted)
            .unwrap_or_else(|| panic!("no {dotted}"))
    }

    #[test]
    fn every_setting_with_its_value_and_whether_its_set() {
        let text = "fit_preview = \"never\"\n[agents.pi]\nargs = [\"--tui-mode\", \"regular\"]\n[agents.aider]\nidle_after = 2\n";
        let config = Config::parse(text).unwrap();
        let sections = sections(&config, text, &cx());
        let fit = find(&sections, "fit_preview");
        assert_eq!((fit.value.as_str(), fit.set), ("never", true));
        let status = find(&sections, "status_line");
        assert_eq!((status.value.as_str(), status.set), ("on", false));
        assert_eq!(find(&sections, "shell").value, "/bin/zsh");
        let args = find(&sections, "agents.pi.args");
        assert_eq!(
            (args.value.as_str(), args.set),
            ("--tui-mode regular", true)
        );
        // Each agent has the settings that apply to it.
        assert_eq!(find(&sections, "agents.claude.title_model").value, "haiku");
        assert_eq!(find(&sections, "agents.pi.auto_title").value, "on");
        assert_eq!(find(&sections, "agents.aider.idle_after").value, "2s");
        assert!(
            sections
                .iter()
                .flat_map(|s| &s.settings)
                .all(|s| s.dotted() != "agents.claude.auto_title")
        );
        assert_eq!(find(&sections, "agents.claude.fit_preview").value, "never");
        assert_eq!(find(&sections, "keys.session.stop").value, "x");
        assert_eq!(find(&sections, "keys.up").value, "k, Up");
    }

    #[test]
    fn a_change_keeps_the_rest_of_the_file() {
        let text = "# My settings.\nfit_preview = \"never\"  # the layout stays\n\n[agents.pi]\n# copy mode\nargs = [\"--tui-mode\", \"regular\"]\n";
        let path = |p: &str| p.split('.').map(String::from).collect::<Vec<_>>();
        let changed = edit(text, &path("status_line"), Some(toml_edit::value(false))).unwrap();
        assert!(
            changed.starts_with(
                "# My settings.\nfit_preview = \"never\"  # the layout stays\nstatus_line = false\n"
            ),
            "{changed}"
        );
        assert!(
            changed.contains("# copy mode\nargs = [\"--tui-mode\", \"regular\"]"),
            "{changed}"
        );
        // A new table for an agent, written as one.
        let changed = edit(
            text,
            &path("agents.claude.title_model"),
            Some(toml_edit::value("sonnet")),
        )
        .unwrap();
        assert!(
            changed.contains("[agents.claude]\ntitle_model = \"sonnet\"\n"),
            "{changed}"
        );
        assert!(!changed.contains("[agents]\n"), "{changed}");
        Config::parse(&changed).unwrap();
        // Back to its default: taken out, with its comment.
        let changed = edit(text, &path("agents.pi.args"), None).unwrap();
        assert!(
            changed.ends_with("the layout stays\n\n[agents.pi]\n"),
            "{changed}"
        );
        let changed = edit(
            "",
            &path("keys.session.stop"),
            Some(parse_input(&Kind::Keys, "y Delete").unwrap()),
        )
        .unwrap();
        assert_eq!(changed, "[keys.session]\nstop = [\"y\", \"Delete\"]\n");
    }

    fn at(dotted: &str) -> Vec<String> {
        dotted.split('.').map(String::from).collect()
    }

    #[test]
    fn a_change_keeps_the_comments_on_it() {
        let text = "# My settings.\n\n# Long lines cut.\nfit_preview = \"never\"  # the layout stays\nstatus_line = true\n";
        let changed = edit(text, &at("fit_preview"), Some(toml_edit::value("auto"))).unwrap();
        assert_eq!(
            changed,
            "# My settings.\n\n# Long lines cut.\nfit_preview = \"auto\"  # the layout stays\nstatus_line = true\n"
        );
        let changed = edit(
            "[agents.pi]\n# copy mode\nargs = [\"--tui-mode\", \"regular\"] # on the normal screen\n",
            &at("agents.pi.args"),
            Some(parse_input(&Kind::Words, "--tui-mode full").unwrap()),
        )
        .unwrap();
        assert_eq!(
            changed,
            "[agents.pi]\n# copy mode\nargs = [\"--tui-mode\", \"full\"] # on the normal screen\n"
        );
    }

    #[test]
    fn taken_out_with_its_comment_and_nothing_else() {
        // The comment just above goes with it; the file's own stays.
        let text = "# My settings.\n\n# Long lines cut.\nfit_preview = \"never\"\n\n# pi on the normal screen.\n[agents.pi]\nargs = [\"--tui-mode\", \"regular\"]\n";
        assert_eq!(
            edit(text, &at("fit_preview"), None).unwrap(),
            "# My settings.\n\n# pi on the normal screen.\n[agents.pi]\nargs = [\"--tui-mode\", \"regular\"]\n"
        );
        // An agent's table stays: it's what makes the agent.
        assert_eq!(
            edit(text, &at("agents.pi.args"), None).unwrap(),
            "# My settings.\n\n# Long lines cut.\nfit_preview = \"never\"\n\n# pi on the normal screen.\n[agents.pi]\n"
        );
        let aider = "[agents.aider]\ncommand = \"aider\"\n";
        assert_eq!(
            edit(aider, &at("agents.aider.command"), None).unwrap(),
            "[agents.aider]\n"
        );
        // Not set: nothing changes.
        assert_eq!(
            edit("[agents.aider]\n", &at("agents.aider.idle_after"), None).unwrap(),
            "[agents.aider]\n"
        );
        // Another table left empty goes, unless something's commented in it.
        let keys =
            "status_line = false\n\n[keys.session]\nstop = \"y\"\n\n[order]\nby = \"name\"\n";
        assert_eq!(
            edit(keys, &at("keys.session.stop"), None).unwrap(),
            "status_line = false\n\n[order]\nby = \"name\"\n"
        );
        let commented = "[keys]\nup = \"k\"\n# down = \"j\"\n";
        assert_eq!(
            edit(commented, &at("keys.up"), None).unwrap(),
            "[keys]\n# down = \"j\"\n"
        );
        // A whole agent, its own lines: what follows it stays.
        let notes = "[agents.aider]\ncommand = \"aider\"\n\n# unrelated\n\n[agents.pi]\nargs = []\n# the end\n";
        assert_eq!(
            edit(notes, &at("agents.aider"), None).unwrap(),
            "# unrelated\n\n[agents.pi]\nargs = []\n# the end\n"
        );
        assert_eq!(
            edit(notes, &at("agents.pi"), None).unwrap(),
            "[agents.aider]\ncommand = \"aider\"\n\n# unrelated\n\n# the end\n"
        );
        // A setting commented out above isn't its comment.
        let template =
            "# General\n# default_agent = \"claude\"    # the agent\nfit_preview = \"never\"\n";
        assert_eq!(
            edit(template, &at("fit_preview"), None).unwrap(),
            "# General\n# default_agent = \"claude\"    # the agent\n"
        );
        // A whole agent, up to the comment on the table after it.
        let two = "[agents.aider]\ncommand = \"aider\"\nargs = [\"--yes\"]\n\n# pi's.\n[agents.pi]\nargs = []\n";
        assert_eq!(
            edit(two, &at("agents.aider"), None).unwrap(),
            "# pi's.\n[agents.pi]\nargs = []\n"
        );
        // Written with dotted keys, too.
        assert_eq!(
            edit(
                "agents.pi.args = [\"x\"]\nfit_preview = \"never\"\n",
                &at("agents.pi.args"),
                None
            )
            .unwrap(),
            "fit_preview = \"never\"\n"
        );
    }

    #[test]
    fn inline_tables_are_tables_too() {
        let text = "[agents]\npi = { args = [\"x\"] }\n";
        let config = Config::parse(text).unwrap();
        let sections = sections(&config, text, &cx());
        assert!(find(&sections, "agents.pi.args").set);
        let changed = edit(
            text,
            &at("agents.pi.title_model"),
            Some(toml_edit::value("small")),
        )
        .unwrap();
        assert_eq!(
            Config::parse(&changed)
                .unwrap()
                .agent("pi")
                .title_model
                .as_deref(),
            Some("small")
        );
        let changed = edit(text, &at("agents.pi.args"), None).unwrap();
        assert!(
            Config::parse(&changed).unwrap().agent("pi").args.is_empty(),
            "{changed}"
        );
    }

    #[test]
    fn a_new_table_goes_after_the_comments_that_end_the_file() {
        let text = "fit_preview = \"never\"\n\n# [order]\n# by = \"state\"\n";
        let changed = edit(
            text,
            &at("agents.aider.command"),
            Some(toml_edit::value("aider")),
        )
        .unwrap();
        assert_eq!(
            changed,
            "fit_preview = \"never\"\n\n# [order]\n# by = \"state\"\n\n[agents.aider]\ncommand = \"aider\"\n"
        );
        // Every setting commented out, as `e` starts the file.
        let template = template(&sections(&Config::default(), "", &cx()));
        let changed = edit(
            &template,
            &at("agents.aider.command"),
            Some(toml_edit::value("aider")),
        )
        .unwrap();
        assert!(changed.starts_with(&template), "{changed}");
        Config::parse(&changed).unwrap();
    }

    #[test]
    fn new_agents_are_named_after_what_they_run() {
        let name = |line: &str| {
            let words = split_words(line).unwrap();
            agent_name(&words[0], &words[1..])
        };
        assert_eq!(name("aider --yes"), "aider");
        assert_eq!(name("/opt/bin/codex"), "codex");
        assert_eq!(name("npx -y @openai/codex@latest"), "codex");
        assert_eq!(name("pipx run gpt-engineer"), "gpt-engineer");
        assert!(!agent_named("a b") && !agent_named("") && agent_named("my-agent_2"));
    }

    #[test]
    fn only_settings_roster_has_are_set() {
        let config = Config::default();
        let sections = sections(&config, "", &cx());
        let kind = |dotted: &str| kind_at(&sections, &at(dotted));
        assert_eq!(kind("status_line"), Some(Kind::Bool));
        // An agent not configured yet: its settings.
        assert_eq!(kind("agents.codex.command"), Some(Kind::Text));
        // Claude names its own titles; not a name.
        assert_eq!(kind("agents.claude.auto_title"), None);
        assert_eq!(kind("agents.a b.command"), None);
        assert_eq!(kind("agents..command"), None);
    }

    #[test]
    fn values_are_typed_as_in_a_shell_or_by_name() {
        assert_eq!(
            split_words(r#"--model "big one" -x 'a b'"#).unwrap(),
            ["--model", "big one", "-x", "a b"]
        );
        assert!(split_words("'open").is_err());
        assert_eq!(
            shell_words(&["--model".into(), "big one".into()]),
            "--model 'big one'"
        );
        assert_eq!(
            parse_input(&Kind::Keys, "").unwrap().to_string().trim(),
            "\"\""
        );
        assert_eq!(
            parse_input(&Kind::Seconds, "5s")
                .unwrap()
                .to_string()
                .trim(),
            "5"
        );
        assert!(parse_input(&Kind::Bool, "maybe").is_err());
        assert!(parse_input(&Kind::Choice(vec!["auto".into()]), "never").is_err());
    }

    #[test]
    fn the_template_has_every_setting_and_reads_as_the_defaults() {
        let config = Config::default();
        let sections = sections(&config, "", &cx());
        let template = template(&sections);
        // Uncommented, it's a valid config, and it's the defaults.
        let uncommented: String = template
            .lines()
            .filter_map(|l| l.strip_prefix("# "))
            .filter(|l| l.contains(" = ") || l.starts_with('['))
            .map(|l| l.split("    # ").next().unwrap().to_string() + "\n")
            .collect();
        let parsed = Config::parse(&uncommented).unwrap_or_else(|e| panic!("{e:#}\n{uncommented}"));
        assert_eq!(
            format!("{:?}", parsed.keys),
            format!("{:?}", Keys::default())
        );
        assert_eq!(parsed.order, Order::default());
        assert!(
            template.contains(
                "# title_model = \"haiku\"    # the model a generated title is made with"
            ),
            "{template}"
        );
    }

    #[test]
    fn the_readme_names_every_setting() {
        let readme = include_str!("../README.md");
        let sections = sections(&Config::default(), "", &cx());
        for s in sections.iter().flat_map(|s| &s.settings) {
            assert!(
                readme.contains(s.name()),
                "the README doesn't mention {}",
                s.dotted()
            );
        }
    }

    #[test]
    fn writing_keeps_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles/config.toml");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, "old = 1\n").unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_file(&link, "new = 2\n").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "new = 2\n");
        // A link to a file that isn't there yet: the file is made.
        let later = dir.path().join("dotfiles/later.toml");
        let dangling = dir.path().join("dangling.toml");
        std::os::unix::fs::symlink(&later, &dangling).unwrap();
        write_file(&dangling, "b = 1\n").unwrap();
        assert!(
            fs::symlink_metadata(&dangling)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&later).unwrap(), "b = 1\n");
        // Not there yet: made.
        let fresh = dir.path().join("fresh/config.toml");
        write_file(&fresh, "a = 1\n").unwrap();
        assert_eq!(fs::read_to_string(&fresh).unwrap(), "a = 1\n");
    }
}
