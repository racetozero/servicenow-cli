//! What `--help` and `introspect` say about each command beyond its one line:
//! `help.txt` has the description, the effect and the examples.

use clap::Command;
use clap::builder::Styles;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::sync::OnceLock;

const TEXT: &str = include_str!("help.txt");

/// What a command does to the instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Read,
    Write,
    Confirm,
    Local,
    ConfirmLocal,
    Depends,
}

impl Effect {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "read" => Self::Read,
            "write" => Self::Write,
            "confirm" => Self::Confirm,
            "local" => Self::Local,
            "confirm-local" => Self::ConfirmLocal,
            "depends" => Self::Depends,
            _ => return None,
        })
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Confirm => "confirm",
            Self::Local => "local",
            Self::ConfirmLocal => "confirm-local",
            Self::Depends => "depends",
        }
    }

    const fn text(self) -> &'static str {
        match self {
            Self::Read => "reads only.",
            Self::Write => {
                "changes the instance. --dry-run shows the request and sends nothing; --read-only refuses it."
            }
            Self::Confirm => {
                "changes the instance and asks first. Agents and scripts need --yes; --dry-run shows the request and sends nothing."
            }
            Self::Local => "does not change records of the instance.",
            Self::ConfirmLocal => {
                "changes files on this computer and asks first. Agents and scripts need --yes."
            }
            Self::Depends => {
                "reads or writes, as you ask. --dry-run shows a write and sends nothing; --read-only refuses it."
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Entry {
    pub effect: Option<Effect>,
    pub description: String,
    /// A note (often empty) and a command line.
    pub examples: Vec<(&'static str, &'static str)>,
}

/// The entry of every command, by its path: `table list`.
pub fn entries() -> HashMap<&'static str, Entry> {
    TEXT.split("## ")
        .skip(1)
        .filter_map(|block| {
            let mut lines = block.lines();
            let path = lines.next()?;
            let mut entry = Entry::default();
            let mut note = "";
            for line in lines.filter(|l| !l.is_empty()) {
                if let Some(name) = line.strip_prefix("effect: ") {
                    entry.effect = Effect::parse(name);
                } else if let Some(text) = line.strip_prefix("> ") {
                    note = text;
                } else if let Some(example) = line.strip_prefix("$ ") {
                    entry.examples.push((std::mem::take(&mut note), example));
                } else {
                    if !entry.description.is_empty() {
                        entry.description.push('\n');
                    }
                    entry.description.push_str(line);
                }
            }
            Some((path, entry))
        })
        .collect()
}

/// The part of `--help` below the options.
pub fn footer(effect: Option<Effect>, examples: &[(&str, &str)]) -> String {
    let header = *Styles::default().get_header();
    let mut out = String::new();
    if let Some(effect) = effect {
        let _ = writeln!(out, "{header}Effect:{header:#} {}\n", effect.text());
    }
    if !examples.is_empty() {
        let _ = writeln!(out, "{header}Examples:{header:#}");
        for (note, example) in examples {
            if !note.is_empty() {
                let _ = writeln!(out, "  # {note}");
            }
            let _ = writeln!(out, "  {example}");
        }
    }
    out
}

/// Adds the long help to the tree. Help only: it costs more than a command
/// that runs needs to pay.
pub fn enrich(root: Command) -> Command {
    let globals = globals(&root);
    let listing = listing(&root);
    let root = root
        .mut_args(|arg| match arg.is_global_set() {
            true => arg.hide(true),
            false => arg,
        })
        .override_usage("sn [OPTIONS] <COMMAND>")
        .help_template(format!(
            "{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{listing}{{after-help}}"
        ))
        .after_help(globals.clone());
    let entries = entries();
    let mut path = String::new();
    root.mut_subcommands(|sub| with_entry(sub, &mut path, &entries, &globals))
}

fn with_entry(
    command: Command,
    path: &mut String,
    entries: &HashMap<&'static str, Entry>,
    globals: &str,
) -> Command {
    let length = path.len();
    if length > 0 {
        path.push(' ');
    }
    path.push_str(command.get_name());
    let mut below = command
        .get_after_long_help()
        .map(|text| text.ansi().to_string())
        .unwrap_or_default();
    let mut command = command;
    if let Some(entry) = entries.get(path.as_str()) {
        let about = command.get_about().map(ToString::to_string);
        command = command.long_about(format!(
            "{}.\n\n{}",
            about.unwrap_or_default().trim_end_matches('.'),
            entry.description
        ));
        below.push_str(&footer(entry.effect, &entry.examples));
    }
    if !below.is_empty() && !below.ends_with("\n\n") {
        below.push('\n');
    }
    command = command
        .after_long_help(format!("{below}{globals}"))
        .mut_subcommands(|sub| with_entry(sub, path, entries, globals));
    path.truncate(length);
    command
}

fn width() -> usize {
    static WIDTH: OnceLock<usize> = OnceLock::new();
    *WIDTH.get_or_init(|| match std::io::stdout().is_terminal() {
        true => ratatui::crossterm::terminal::size()
            .map_or(100, |(columns, _)| usize::from(columns).clamp(60, 110)),
        false => 100,
    })
}

/// A row of a two-column list, wrapped in the second column.
fn row(out: &mut String, left: &str, right: &str, indent: usize) {
    let room = width().saturating_sub(indent).max(30);
    let mut column = 2 + left.len();
    let _ = write!(out, "  {left}");
    let mut used = room + 1;
    for (index, word) in right.split_whitespace().enumerate() {
        if used + 1 + word.len() > room {
            if index > 0 || column + 1 > indent {
                out.push('\n');
                column = 0;
            }
            let _ = write!(out, "{:1$}{word}", "", indent - column);
            used = word.len();
        } else {
            let _ = write!(out, " {word}");
            used += 1 + word.len();
        }
    }
    out.push('\n');
}

/// The global options, one or two lines each: every command repeats them.
fn globals(root: &Command) -> String {
    let header = *Styles::default().get_header();
    let mut out = format!("{header}Global options:{header:#}\n");
    for arg in root.get_arguments().filter(|a| a.is_global_set()) {
        let short = arg
            .get_short()
            .map_or_else(|| "    ".to_owned(), |s| format!("-{s}, "));
        let mut left = format!("{short}--{}", arg.get_long().unwrap_or_default());
        if arg.get_action().takes_values()
            && !arg.get_num_args().is_some_and(|n| n.min_values() == 0)
        {
            let value = arg.get_value_names().and_then(<[_]>::first);
            let _ = write!(left, " <{}>", value.map_or("VALUE", |v| v.as_str()));
        }
        let mut right = arg.get_help().map(ToString::to_string).unwrap_or_default();
        if arg.get_action().takes_values() && arg.get_long() == Some("output") {
            let values: Vec<String> = arg
                .get_possible_values()
                .iter()
                .map(|v| v.get_name().to_owned())
                .collect();
            let _ = write!(right, ": {}", values.join(", "));
        }
        if let Some(name) = arg.get_env() {
            let _ = write!(right, " [env: {}]", name.to_string_lossy());
        }
        row(&mut out, &left, &right, 28);
    }
    out
}

/// The commands by area.
fn listing(root: &Command) -> String {
    const AREAS: [(&str, std::ops::Range<usize>); 6] = [
        ("Start", 0..10),
        ("Records", 10..20),
        ("Service management", 20..30),
        ("Development", 30..40),
        ("Any API", 40..50),
        ("Setup and tools", 50..1000),
    ];
    let header = *Styles::default().get_header();
    let mut out = String::new();
    for (title, range) in AREAS {
        let _ = writeln!(out, "{header}{title}:{header:#}");
        for command in root
            .get_subcommands()
            .filter(|c| !c.is_hide_set() && range.contains(&c.get_display_order()))
        {
            let mut about = command
                .get_about()
                .map_or_else(String::new, ToString::to_string);
            if let Some(alias) = command.get_visible_aliases().next() {
                let _ = write!(about, " [alias: {alias}]");
            }
            row(&mut out, command.get_name(), &about, 15);
        }
        out.push('\n');
    }
    // The template adds an empty line of its own.
    out.truncate(out.trim_end().len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli;

    fn walk(command: &Command, path: &str, seen: &mut Vec<(String, bool)>) {
        for sub in command.get_subcommands().filter(|s| !s.is_hide_set()) {
            let path = format!("{path} {}", sub.get_name()).trim().to_owned();
            for arg in sub.get_arguments().filter(|a| !a.is_hide_set()) {
                let id = arg.get_id();
                assert!(arg.get_help().is_some(), "{path}: {id} has no help");
            }
            let leaf = !sub.get_subcommands().any(|s| !s.is_hide_set());
            seen.push((path.clone(), leaf));
            walk(sub, &path, seen);
        }
    }

    #[test]
    fn every_command_has_an_effect_a_description_and_examples() {
        let mut seen = Vec::new();
        let argv = ["sn", "api"].map(String::from);
        walk(&cli::with_api(cli::root(), &argv), "", &mut seen);
        let mut entries = entries();
        for (path, leaf) in seen {
            let entry = entries.remove(path.as_str());
            let entry = entry.unwrap_or_else(|| panic!("help.txt has no `## {path}`"));
            assert!(!entry.description.is_empty(), "{path}: no description");
            assert!(!entry.examples.is_empty(), "{path}: no example");
            assert!(!leaf || entry.effect.is_some(), "{path}: no effect");
            for (_, example) in entry.examples {
                // A longer line wraps and does not paste.
                assert!(example.len() <= 98, "{path}: shorten `{example}`");
            }
        }
        assert!(entries.is_empty(), "no such commands: {:?}", entries.keys());
    }

    #[test]
    fn every_example_parses_as_its_command() {
        for (path, entry) in entries() {
            for (_, example) in entry.examples {
                let line = example
                    .strip_prefix("eval \"$(")
                    .map_or(example, |l| l.trim_end_matches(")\""));
                let line = line.split(" > ").next().unwrap_or(line);
                let line = line.split(" < ").next().unwrap_or(line);
                let argv = crate::cmd::profile::split_words(line).expect(example);
                let name = cli::parse_name(argv).unwrap_or_else(|err| panic!("{example}: {err}"));
                assert!(
                    entry.effect.is_none() || name.starts_with(path),
                    "`{example}` of {path} runs `{name}`"
                );
            }
        }
    }
}
