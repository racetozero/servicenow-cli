//! Output for people, scripts and agents.

use jaq_core::data::JustLut;
use serde_json::{Map, Value, json};
use sn_core::{Error, Result};
use std::borrow::Cow;
use std::cell::OnceCell;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

static AGENT: OnceLock<bool> = OnceLock::new();
static COMMAND: RwLock<String> = RwLock::new(String::new());
static NO_COLOR: AtomicBool = AtomicBool::new(false);

/// Set once, before anything can fail.
pub fn set_agent(agent: bool) {
    let _ = AGENT.set(agent);
}

pub fn set_command(command: String) {
    if let Ok(mut current) = COMMAND.write() {
        *current = command;
    }
}

/// `--no-color`, and agent mode.
pub fn disable_color() {
    NO_COLOR.store(true, Ordering::Relaxed);
}

pub fn agent_mode() -> bool {
    AGENT.get().copied().unwrap_or(false)
}

pub fn command_name() -> String {
    COMMAND.read().map(|c| c.clone()).unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Format {
    /// Tables and text in a terminal, JSON in a pipe.
    #[default]
    Auto,
    Table,
    Json,
    /// One JSON value per line.
    #[value(alias = "ndjson")]
    Jsonl,
    Yaml,
    Csv,
    Tsv,
}

/// A table or CSV column: the header and the record key it shows.
#[derive(Debug, Clone)]
pub struct Col {
    pub header: Cow<'static, str>,
    pub key: Cow<'static, str>,
    /// Takes the remaining terminal width.
    pub flex: bool,
}

pub const fn col(header: &'static str, key: &'static str) -> Col {
    Col {
        header: Cow::Borrowed(header),
        key: Cow::Borrowed(key),
        flex: false,
    }
}

pub const fn flex(header: &'static str, key: &'static str) -> Col {
    Col {
        header: Cow::Borrowed(header),
        key: Cow::Borrowed(key),
        flex: true,
    }
}

/// Columns for field names, the longest text field flexible.
pub fn columns<S: AsRef<str>>(fields: &[S]) -> Vec<Col> {
    let wide = [
        "short_description",
        "description",
        "name",
        "value",
        "label",
        "title",
        "message",
    ];
    let flex_at = wide
        .iter()
        .find_map(|w| fields.iter().position(|f| f.as_ref() == *w));
    fields
        .iter()
        .enumerate()
        .map(|(i, field)| Col {
            header: field.as_ref().to_ascii_uppercase().into(),
            key: field.as_ref().to_owned().into(),
            flex: Some(i) == flex_at,
        })
        .collect()
}

/// The columns of rows whose shape is only known at run time.
pub fn infer_columns(rows: &[Value]) -> Vec<Col> {
    let mut fields: Vec<String> = Vec::new();
    for row in rows.iter().take(50) {
        for key in row.as_object().into_iter().flat_map(Map::keys) {
            if !fields.contains(key) {
                fields.push(key.clone());
            }
        }
    }
    // Wide records do not fit a terminal: keep what identifies them.
    if fields.len() > 9 {
        let first = [
            "number",
            "name",
            "label",
            "user_name",
            "title",
            "short_description",
            "priority",
            "state",
            "assigned_to",
            "assignment_group",
            "sys_class_name",
            "active",
            "sys_updated_on",
            "sys_id",
        ];
        let mut kept: Vec<String> = first
            .iter()
            .filter(|f| fields.iter().any(|have| have == *f))
            .map(|f| (*f).to_owned())
            .collect();
        if kept.len() < 3 {
            kept = fields.into_iter().take(8).collect();
        }
        kept.truncate(8);
        fields = kept;
    }
    columns(&fields)
}

pub struct Out {
    pub agent: bool,
    pub format: Format,
    pub select: Vec<String>,
    pub jq: Option<Jq>,
    /// Keep responses as the server sent them, `{"result": ...}` included.
    pub raw: bool,
    pub tty: bool,
    pub no_headers: bool,
    pub no_truncate: bool,
    pub quiet: bool,
    pub pager: bool,
}

impl Out {
    /// Human text and tables, not JSON.
    pub fn human(&self) -> bool {
        !self.agent && (self.format == Format::Table || (self.format == Format::Auto && self.tty))
    }

    /// The command may print its own text: nothing reshapes the data.
    pub fn wants_text(&self) -> bool {
        self.human() && self.select.is_empty() && self.jq.is_none()
    }

    /// A status line for people on stderr.
    pub fn note(&self, text: &str) {
        if !self.quiet && !self.agent {
            let _ = writeln!(std::io::stderr(), "{text}");
        }
    }

    /// A warning: stderr for everyone, also in agent mode.
    pub fn warn(&self, text: &str) {
        let _ = writeln!(std::io::stderr(), "warning: {text}");
    }

    fn envelope(&self, data: Value, meta: Value) -> bool {
        let mut out = json!({ "ok": true, "command": command_name(), "data": data });
        if meta.as_object().is_some_and(|m| !m.is_empty()) {
            out["meta"] = meta;
        }
        print_line(&out.to_string())
    }

    fn filtered(&self, filter: &Jq, data: &Value, meta: Value) -> Result<bool> {
        let mut results = filter.run(data)?;
        if self.agent {
            let data = match results.len() {
                1 => results.remove(0),
                _ => Value::Array(results),
            };
            return Ok(self.envelope(data, meta));
        }
        Ok(results.iter().all(|result| match result {
            Value::String(text) => print_line(text),
            other => print_line(&other.to_string()),
        }))
    }

    fn machine(&self, data: &Value) -> Result<()> {
        match (self.format, data) {
            (Format::Jsonl, Value::Array(items)) => {
                items.iter().all(|item| print_line(&item.to_string()))
            }
            (Format::Jsonl, other) => print_line(&other.to_string()),
            (Format::Yaml, data) => {
                let text = serde_yaml_ng::to_string(data)
                    .map_err(|err| Error::Internal(format!("cannot write YAML: {err}")))?;
                print_line(text.trim_end())
            }
            _ if self.tty => print_line(&serde_json::to_string_pretty(data)?),
            _ => print_line(&data.to_string()),
        };
        Ok(())
    }

    /// A list of records. Empty `cols` are inferred from the rows. `meta`
    /// is an object for the agent envelope; its `next` key is also shown to
    /// people as the way to more results.
    pub fn list(&self, rows: Vec<Value>, cols: &[Col], meta: Value) -> Result<()> {
        let rows: Vec<Value> = rows
            .into_iter()
            .map(|row| select(row, &self.select))
            .collect();
        if let Some(filter) = &self.jq {
            self.filtered(filter, &Value::Array(rows), meta)?;
            return Ok(());
        }
        if self.agent {
            let mut meta = if meta.is_object() { meta } else { json!({}) };
            meta["count"] = rows.len().into();
            self.envelope(Value::Array(rows), meta);
            return Ok(());
        }
        let chosen: Vec<Col> = match (self.select.is_empty(), cols.is_empty()) {
            (false, _) => columns(&self.select),
            (true, true) => infer_columns(&rows),
            (true, false) => cols.to_vec(),
        };
        match self.format {
            Format::Csv | Format::Tsv => self.delimited(&rows, &chosen),
            _ if self.human() => {
                if rows.is_empty() {
                    self.note(&dim("no results", true));
                    return Ok(());
                }
                print_table(&rows, &chosen, self);
                if let Some(next) = meta.get("next").and_then(Value::as_str) {
                    self.note(&dim(&format!("more results: {next}"), true));
                }
                Ok(())
            }
            _ => self.machine(&Value::Array(rows)),
        }
    }

    /// True when records go out as JSON and nothing looks inside them.
    pub fn passes_through(&self) -> bool {
        let json = matches!(self.format, Format::Auto | Format::Json | Format::Jsonl);
        (self.agent || json && !self.tty)
            && self.select.is_empty()
            && self.jq.is_none()
            && !self.raw
    }

    /// The same as [`Out::list`] for records as the server sent them, when
    /// [`Out::passes_through`]: a page of 1000 records is neither parsed into
    /// a tree nor printed from one.
    pub fn list_verbatim(
        &self,
        rows: &[&serde_json::value::RawValue],
        mut meta: Value,
    ) -> Result<()> {
        let mut text =
            String::with_capacity(rows.iter().map(|r| r.get().len() + 1).sum::<usize>() + 256);
        let lines = !self.agent && self.format == Format::Jsonl;
        if self.agent {
            meta["count"] = rows.len().into();
            text.push_str(&format!(
                "{{\"ok\":true,\"command\":{},\"data\":",
                json!(command_name())
            ));
        }
        if !lines {
            text.push('[');
        }
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                text.push(if lines { '\n' } else { ',' });
            }
            text.push_str(row.get());
        }
        if !lines {
            text.push(']');
        }
        if self.agent {
            text.push_str(&format!(",\"meta\":{meta}}}"));
        }
        if !(lines && rows.is_empty()) {
            print_line(&text);
        }
        Ok(())
    }

    fn delimited(&self, rows: &[Value], cols: &[Col]) -> Result<()> {
        let tsv = self.format == Format::Tsv;
        let mut writer = csv::WriterBuilder::new()
            .delimiter(if tsv { b'\t' } else { b',' })
            // TSV has no quoting: tabs and line breaks become spaces.
            .quote_style(if tsv {
                csv::QuoteStyle::Never
            } else {
                csv::QuoteStyle::Necessary
            })
            .terminator(csv::Terminator::Any(b'\n'))
            .from_writer(Vec::new());
        let clean = |text: String| match tsv {
            true => text.replace(['\t', '\n', '\r'], " "),
            false => text,
        };
        let failed = |err: csv::Error| Error::Internal(format!("cannot write the rows: {err}"));
        if !self.no_headers {
            writer
                .write_record(cols.iter().map(|c| c.key.as_ref()))
                .map_err(failed)?;
        }
        for row in rows {
            writer
                .write_record(cols.iter().map(|c| clean(cell(row, &c.key))))
                .map_err(failed)?;
        }
        let bytes = writer
            .into_inner()
            .map_err(|err| Error::Internal(format!("cannot write the rows: {err}")))?;
        print_line(String::from_utf8_lossy(&bytes).trim_end_matches('\n'));
        Ok(())
    }

    /// One record, shown to people as a field list.
    pub fn item(&self, data: Value) -> Result<()> {
        self.item_with(data, json!({}), fields_text)
    }

    /// One record with its own text for people.
    pub fn item_with(
        &self,
        data: Value,
        meta: Value,
        human: impl FnOnce(&Value) -> String,
    ) -> Result<()> {
        let data = select(data, &self.select);
        if let Some(filter) = &self.jq {
            self.filtered(filter, &data, meta)?;
            return Ok(());
        }
        if self.agent {
            self.envelope(data, meta);
            return Ok(());
        }
        if self.human() {
            let text = if self.select.is_empty() {
                human(&data)
            } else {
                fields_text(&data)
            };
            let text = text.trim_end();
            if !(self.pager && self.tty && std::io::stdin().is_terminal() && page(text)) {
                print_line(text);
            }
            return Ok(());
        }
        if matches!(self.format, Format::Csv | Format::Tsv) {
            let rows = [data];
            let cols = match self.select.is_empty() {
                true => infer_columns(&rows),
                false => columns(&self.select),
            };
            return self.delimited(&rows, &cols);
        }
        self.machine(&data)
    }

    /// The result of a write: `text` for people, `data` for machines.
    pub fn done(&self, data: Value, text: &str) -> Result<()> {
        self.item_with(data, json!({}), |_| text.to_owned())
    }

    /// Data of any shape: arrays become tables, objects field lists.
    pub fn any(&self, data: Value) -> Result<()> {
        match data {
            Value::Array(rows) if rows.iter().all(Value::is_object) => {
                self.list(rows, &[], json!({}))
            }
            other => self.item(other),
        }
    }

    /// Plain text: a script, XML, YAML. Agents get it in the envelope.
    pub fn text(&self, text: &str) -> Result<()> {
        if self.agent {
            self.envelope(text.into(), json!({}));
        } else {
            print_line(text.trim_end_matches('\n'));
        }
        Ok(())
    }

    /// One event of a stream; agents get one envelope per line. False when
    /// nobody reads any more (`| head`): the stream is over.
    pub fn event(&self, data: Value) -> Result<bool> {
        let data = select(data, &self.select);
        if let Some(filter) = &self.jq {
            return self.filtered(filter, &data, json!({}));
        }
        Ok(match self.agent {
            true => self.envelope(data, json!({})),
            false => print_line(&data.to_string()),
        })
    }
}

/// The request that `--dry-run` did not send.
pub fn dry_run(request: &Value) {
    if agent_mode() {
        print_line(
            &json!({ "ok": true, "command": command_name(), "data": { "dry_run": true, "request": request } })
                .to_string(),
        );
    } else {
        let text = serde_json::to_string_pretty(request).unwrap_or_default();
        print_line(&text);
        let _ = writeln!(
            std::io::stderr(),
            "{}",
            dim("dry run: this request was not sent", true)
        );
    }
}

/// Prints and flushes a line. False when that did not work: a closed pipe
/// (`| head`) is not an error worth reporting.
pub fn print_line(text: &str) -> bool {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{text}").and_then(|()| out.flush()).is_ok()
}

/// Keeps only the requested keys; dot paths reach into objects, and a key
/// that contains dots (a dot-walked field) is found as it is.
pub fn select(value: Value, keys: &[String]) -> Value {
    if keys.is_empty() {
        return value;
    }
    match value {
        Value::Array(items) => items.into_iter().map(|item| select(item, keys)).collect(),
        Value::Object(_) => Value::Object(
            keys.iter()
                .map(|key| {
                    (
                        key.clone(),
                        lookup(&value, key).cloned().unwrap_or(Value::Null),
                    )
                })
                .collect(),
        ),
        other => other,
    }
}

fn lookup<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value
        .get(key)
        .or_else(|| value.pointer(&format!("/{}", key.replace('.', "/"))))
}

/// A value as one table cell.
pub fn cell(row: &Value, key: &str) -> String {
    lookup(row, key).map(cell_text).unwrap_or_default()
}

pub fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(items) if items.iter().all(|i| !i.is_object()) => {
            items.iter().map(cell_text).collect::<Vec<_>>().join(", ")
        }
        // Reference and `--display-value all` fields.
        Value::Object(map) if map.contains_key("display_value") || map.contains_key("value") => map
            .get("display_value")
            .filter(|v| !v.is_null())
            .or_else(|| map.get("value"))
            .map(cell_text)
            .unwrap_or_default(),
        other => other.to_string(),
    }
}

/// An object as aligned `field  value` lines; empty fields are left out.
pub fn fields_text(data: &Value) -> String {
    let tty = std::io::stdout().is_terminal();
    match data {
        Value::Object(map) => {
            let shown: Vec<(&String, String)> = map
                .iter()
                .map(|(k, v)| (k, cell_text(v)))
                .filter(|(_, v)| !v.is_empty())
                .collect();
            let width = shown
                .iter()
                .map(|(k, _)| k.len())
                .max()
                .unwrap_or(0)
                .min(32);
            shown
                .iter()
                .map(|(key, value)| {
                    let indent = format!("\n{}", " ".repeat(width + 2));
                    let value = value.trim_end().replace('\n', &indent);
                    format!("{}  {value}", dim(&format!("{key:<width$}"), tty))
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        Value::Array(items) => items
            .iter()
            .map(fields_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Null => "done".to_owned(),
        other => cell_text(other),
    }
}

fn print_table(rows: &[Value], cols: &[Col], out: &Out) {
    let width = terminal_size().map_or(120, |(width, _)| width);
    for (index, line) in table_lines(rows, cols, width, out.no_headers, out.no_truncate)
        .iter()
        .enumerate()
    {
        match index == 0 && !out.no_headers {
            true => print_line(&bold(line, out.tty)),
            false => print_line(line),
        };
    }
}

/// Aligned lines: one per row, cells cut with `…`.
pub fn table_lines(
    rows: &[Value],
    cols: &[Col],
    width: u16,
    no_headers: bool,
    no_truncate: bool,
) -> Vec<String> {
    use comfy_table::{CellAlignment, ColumnConstraint, ContentArrangement, Row, Table, Width};
    let mut table = Table::new();
    table
        .load_style(comfy_table::presets::NOTHING)
        .set_truncation_indicator("…")
        .set_content_arrangement(match no_truncate {
            true => ContentArrangement::Disabled,
            false => ContentArrangement::Dynamic,
        })
        .set_width(width);
    if !no_headers {
        table.set_header(cols.iter().map(|c| c.header.as_ref()));
    }
    for row in rows {
        let mut cells = Row::from(
            cols.iter()
                .map(|c| cell(row, &c.key).replace(['\n', '\r', '\t'], " ")),
        );
        cells.max_height(1);
        table.add_row(cells);
    }
    let natural = table.column_max_content_widths();
    let last = cols.len().saturating_sub(1);
    for (index, column) in table.column_iter_mut().enumerate() {
        let padding = if index == last { 0 } else { 2 };
        column.set_padding((0, padding));
        column.set_cell_alignment(CellAlignment::Left);
        if no_truncate {
            continue;
        }
        column.set_constraint(match cols[index].flex {
            true => ColumnConstraint::LowerBoundary(Width::Fixed(20 + padding)),
            false => {
                let width = Width::Fixed(natural[index].min(40) + padding);
                ColumnConstraint::Boundaries {
                    lower: width,
                    upper: width,
                }
            }
        });
    }
    table
        .lines()
        .map(|line| line.trim_end().to_owned())
        .collect()
}

/// The terminal's columns and rows. Without a terminal there is none:
/// crossterm would then run `tput` twice.
pub fn terminal_size() -> Option<(u16, u16)> {
    let attached = std::io::stdout().is_terminal() || std::io::stderr().is_terminal();
    attached
        .then(|| ratatui::crossterm::terminal::size().ok())
        .flatten()
}

/// Shows `text` in the pager when it is longer than the terminal.
fn page(text: &str) -> bool {
    let rows = terminal_size().map_or(usize::MAX, |(_, h)| h as usize);
    if text.lines().count() < rows {
        return false;
    }
    let command = ["SN_PAGER", "PAGER"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .unwrap_or_else(|| {
            if cfg!(unix) {
                "less -FRX".into()
            } else {
                String::new()
            }
        });
    let mut words = command.split_whitespace();
    let Some(program) = words.next().filter(|p| *p != "cat") else {
        return false;
    };
    let Ok(mut child) = std::process::Command::new(program)
        .args(words)
        .stdin(std::process::Stdio::piped())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        // The reader may quit early; that is not an error.
        let _ = writeln!(stdin, "{text}");
    }
    let _ = child.wait();
    true
}

/// ANSI styles only on a terminal, and not with `NO_COLOR` or `TERM=dumb`.
fn styled(tty: bool) -> bool {
    static PLAIN: OnceLock<bool> = OnceLock::new();
    tty && !NO_COLOR.load(Ordering::Relaxed)
        && !*PLAIN.get_or_init(|| {
            std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
                || std::env::var_os("TERM").is_some_and(|term| term == "dumb")
        })
}

pub fn paint(code: &str, text: &str, tty: bool) -> String {
    match styled(tty) {
        true => format!("\x1b[{code}m{text}\x1b[0m"),
        false => text.to_owned(),
    }
}

pub fn bold(text: &str, tty: bool) -> String {
    paint("1", text, tty)
}

pub fn dim(text: &str, tty: bool) -> String {
    paint("2", text, tty)
}

type Compiled = jaq_core::compile::Filter<jaq_core::Native<JustLut<jaq_json::Val>>>;

/// A jq filter, compiled at its first use: that reads the whole standard
/// library of jq.
pub struct Jq {
    text: String,
    compiled: OnceCell<Compiled>,
}

impl Jq {
    pub fn new(text: String) -> Self {
        Self {
            text,
            compiled: OnceCell::new(),
        }
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::invalid(format!(
            "--jq `{}` {reason}; use jq syntax such as '.[] | .number' or 'length'",
            self.text
        ))
    }

    fn compile(&self) -> Result<Compiled> {
        use jaq_core::load::{Arena, File, Loader};
        let defs = jaq_core::defs()
            .chain(jaq_std::defs())
            .chain(jaq_json::defs());
        let funs = jaq_core::funs()
            .chain(jaq_std::funs())
            .chain(jaq_json::funs());
        let arena = Arena::default();
        let modules = Loader::new(defs)
            .load(
                &arena,
                File {
                    code: self.text.as_str(),
                    path: (),
                },
            )
            .map_err(|errs| {
                let kinds: Vec<String> = errs.iter().map(|(_, err)| format!("{err:?}")).collect();
                self.invalid(&format!("does not parse ({})", kinds.join("; ")))
            })?;
        jaq_core::Compiler::default()
            .with_funs(funs)
            .compile(modules)
            .map_err(|errs| {
                let kinds: Vec<String> = errs.iter().map(|(_, err)| format!("{err:?}")).collect();
                self.invalid(&format!("does not compile ({})", kinds.join("; ")))
            })
    }

    /// Every result for this input.
    fn run(&self, input: &Value) -> Result<Vec<Value>> {
        let input = jaq_json::read::parse_single(input.to_string().as_bytes())
            .map_err(|err| self.invalid(&format!("cannot read the data ({err})")))?;
        let compiled = match self.compiled.get() {
            Some(compiled) => compiled,
            None => {
                let compiled = self.compile()?;
                self.compiled.get_or_init(|| compiled)
            }
        };
        let ctx =
            jaq_core::Ctx::<JustLut<jaq_json::Val>>::new(&compiled.lut, jaq_core::Vars::new([]));
        compiled
            .id
            .run((ctx, input))
            .map(jaq_core::unwrap_valr)
            .map(|result| {
                let value = result.map_err(|err| self.invalid(&format!("failed: {err}")))?;
                serde_json::from_str(&value.to_string()).map_err(|err| {
                    self.invalid(&format!("produced a value that is not JSON ({err})"))
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_reaches_nested_and_dotted_keys() {
        let rows = json!([{ "a": { "b": 1 }, "caller_id.email": "x" }]);
        assert_eq!(
            select(
                rows,
                &["a.b".into(), "caller_id.email".into(), "none".into()]
            ),
            json!([{ "a.b": 1, "caller_id.email": "x", "none": null }])
        );
    }

    #[test]
    fn cells_show_display_values() {
        let row =
            json!({ "s": { "display_value": "New", "value": "1" }, "n": null, "l": ["a", "b"] });
        assert_eq!(cell(&row, "s"), "New");
        assert_eq!(cell(&row, "n"), "");
        assert_eq!(cell(&row, "l"), "a, b");
    }

    #[test]
    fn tables_fit_the_terminal() {
        let rows =
            vec![json!({ "number": "INC1", "short_description": "日本語の説明 ".repeat(20) })];
        let cols = columns(&["number", "short_description"]);
        let lines = table_lines(&rows, &cols, 60, false, false);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'), "{}", lines[1]);
        assert!(table_lines(&rows, &cols, 60, true, true)[0].len() > 200);
    }

    #[test]
    fn jq_runs_and_explains_mistakes() {
        let filter = Jq::new(".[0].a".into());
        for a in [1, 2] {
            assert_eq!(
                filter.run(&json!([{ "a": a }])).expect("run"),
                [json!(a)],
                "a filter runs again"
            );
        }
        assert!(
            Jq::new(".[".into())
                .run(&json!([]))
                .expect_err("bad")
                .to_string()
                .starts_with("--jq `.[` does not parse")
        );
    }

    #[test]
    fn wide_records_keep_identifying_columns() {
        let mut row = Map::new();
        for field in [
            "z1",
            "z2",
            "z3",
            "z4",
            "z5",
            "z6",
            "z7",
            "z8",
            "number",
            "state",
            "short_description",
        ] {
            row.insert(field.into(), "x".into());
        }
        let keys: Vec<String> = infer_columns(&[row.into()])
            .iter()
            .map(|c| c.key.to_string())
            .collect();
        assert_eq!(keys, ["number", "short_description", "state"]);
    }
}
