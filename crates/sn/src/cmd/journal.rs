//! Comments and work notes.
//!
//! `sys_journal_field`, the table of entries, is closed to most users. The
//! rendered stream in the record's own `comments` and `work_notes` fields is
//! open to everyone who can read the record, so that is the default source.

use super::args::{RecordArgs, dropped};
use super::graphql;
use crate::ctx::Ctx;
use crate::output::{col, flex};
use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::{Value, json};
use sn_core::{Client, Error, Result, query};

#[derive(Args, Debug)]
pub struct JournalArgs {
    #[command(flatten)]
    record: RecordArgs,
    /// Only comments, which the customer can see.
    #[arg(long, conflicts_with = "work_notes")]
    comments: bool,
    /// Only work notes.
    #[arg(long, conflicts_with = "comments")]
    work_notes: bool,
    /// Read the record's rendered stream (your time zone), or the entry table (UTC, user names, admin-only by default).
    #[arg(long, value_enum, default_value_t = Source::Record)]
    source: Source,
    /// Only the newest N entries.
    #[arg(long, short = 'n', value_name = "N")]
    limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Source {
    Record,
    Table,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Entry {
    pub created_on: String,
    pub author: String,
    /// `comments` or `work_notes`, when the label says which.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub text: String,
}

const COMMENTS: &str = "comments";
const WORK_NOTES: &str = "work_notes";

pub async fn run(ctx: &Ctx, args: JournalArgs) -> Result<()> {
    if ctx.out.raw && (args.source == Source::Table || args.limit.is_some()) {
        return Err(Error::invalid(
            "you passed --raw with --source table or --limit; --raw prints the whole rendered stream of the record, so leave the other flag out",
        ));
    }
    let client = ctx.client().await?;
    let record = args.record.record(client).await?;
    let only = match (args.comments, args.work_notes) {
        (true, _) => Some(COMMENTS),
        (_, true) => Some(WORK_NOTES),
        _ => None,
    };
    let mut entries = match args.source {
        Source::Record => {
            let streams = streams(client, &record.table, &record.sys_id, only).await?;
            if ctx.out.raw {
                return ctx.out.text(&streams.concat());
            }
            parse(&streams)?
        }
        Source::Table => rows(client, &record.sys_id, only, args.limit).await?,
    };
    entries.truncate(args.limit.unwrap_or(usize::MAX));
    if ctx.out.wants_text() && !entries.is_empty() {
        let text = text(&entries);
        return ctx
            .out
            .item_with(serde_json::to_value(entries)?, json!({}), |_| text);
    }
    let rows = entries
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<_, _>>()?;
    let cols = [
        col("CREATED", "created_on"),
        col("AUTHOR", "author"),
        col("FIELD", "element"),
        flex("TEXT", "text"),
    ];
    ctx.out.list(rows, &cols, json!({}))
}

/// The entries as people read them.
pub fn text(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|e| {
            let label = e.label.as_deref().or(e.element).unwrap_or("note");
            let body = e.text.replace('\n', "\n  ");
            format!("{}  {} ({label})\n  {body}", e.created_on, e.author)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Entries of a record for a composite read: a table without journal fields
/// has none.
pub async fn entries(client: &Client, table: &str, sys_id: &str) -> Result<Vec<Entry>> {
    match read(client, table, sys_id, &[COMMENTS, WORK_NOTES]).await? {
        Ok(streams) => parse(&streams),
        Err(_) => Ok(Vec::new()),
    }
}

/// The rendered streams. Which fields exist differs by table, and one that
/// does not exist fails the document on older releases.
async fn streams(
    client: &Client,
    table: &str,
    sys_id: &str,
    only: Option<&str>,
) -> Result<Vec<String>> {
    let mut found = match only {
        Some(field) => read(client, table, sys_id, &[field]).await?,
        None => read(client, table, sys_id, &["comments_and_work_notes"]).await?,
    };
    if found.is_err() && only.is_none() {
        found = read(client, table, sys_id, &[COMMENTS, WORK_NOTES]).await?;
    }
    found.map_err(|answer| {
        let what = only.unwrap_or("comments or work_notes");
        Error::api(
            format!("you asked for the journal of a `{table}` record, but this table has no {what} field; check the table"),
            answer,
        )
    })
}

/// The streams of the given fields, without those the table does not have.
/// The inner `Err` holds the answer when none exists.
async fn read(
    client: &Client,
    table: &str,
    sys_id: &str,
    fields: &[&str],
) -> Result<Result<Vec<String>, Value>> {
    let mut fields = fields.to_vec();
    loop {
        let selection: String = fields
            .iter()
            .map(|f| format!("{f} {{ displayValue }} "))
            .collect();
        let document = format!(
            "query ($id: String!) {{ GlideRecord_Query {{ {table}(sys_id: $id) {{ _results {{ {selection}}} }} }} }}"
        );
        let answer = graphql::ask(client, &document, json!({ "id": sys_id })).await?;
        if !graphql::errors(&answer).is_empty() {
            if graphql::unknown_table(&answer, table) {
                return Err(graphql::unknown_table_error(&answer, table));
            }
            let before = fields.len();
            fields.retain(|f| !graphql::undefined(&answer, f));
            if fields.is_empty() {
                return Ok(Err(answer));
            }
            if fields.len() == before {
                return Err(graphql::failure(&answer));
            }
            continue;
        }
        let record = &answer["data"]["GlideRecord_Query"][table]["_results"][0];
        if record.is_null() {
            return Err(Error::NotFound(format!(
                "there is no {table} record {sys_id} that you can read; check the reference and your access"
            )));
        }
        return Ok(Ok(fields
            .iter()
            .map(|f| {
                record[*f]["displayValue"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()));
    }
}

/// The rows of `sys_journal_field`, newest first.
async fn rows(
    client: &Client,
    sys_id: &str,
    only: Option<&str>,
    limit: Option<usize>,
) -> Result<Vec<Entry>> {
    let record = query::term("element_id", "=", sys_id);
    let element = only
        .map(|f| query::term("element", "=", f))
        .unwrap_or_default();
    let conditions = query::all([record.as_str(), &element, "ORDERBYDESCsys_created_on"]);
    let document = "query ($qc: String!, $limit: Int) { GlideRecord_Query { sys_journal_field(queryConditions: $qc, pagination: { limit: $limit }) { _rowCount _results { element_id { value } element { value } value { value } sys_created_on { value } sys_created_by { value } } } } }";
    let variables = json!({ "qc": conditions, "limit": limit.unwrap_or(1000) });
    let answer = graphql::execute(client, document, variables).await?;
    let found = &answer["data"]["GlideRecord_Query"]["sys_journal_field"];
    let results = found["_results"].as_array().map_or(&[][..], Vec::as_slice);
    let count = found["_rowCount"].as_u64().unwrap_or(0);
    // Access rules hide the rows but not their number.
    if count > 0 && results.is_empty() {
        return Err(Error::api(
            format!(
                "the record has {count} journal entries, but you may not read `sys_journal_field`; leave out --source table to read the record's own stream"
            ),
            Value::Null,
        ));
    }
    results
        .iter()
        .map(|row| {
            let field = |name: &str| row[name]["value"].as_str().unwrap_or_default();
            let of = field("element_id");
            if !of.is_empty() && of != sys_id {
                return Err(dropped("element_id", "sys_journal_field"));
            }
            Ok(Entry {
                created_on: field("sys_created_on").to_owned(),
                author: field("sys_created_by").to_owned(),
                element: element_of(field("element")),
                label: None,
                text: field("value").to_owned(),
            })
        })
        .collect()
}

/// Splits rendered streams into entries, newest first.
pub fn parse<S: AsRef<str>>(streams: &[S]) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for stream in streams {
        parse_stream(stream.as_ref(), &mut entries)?;
    }
    // Only dates that sort as text are sorted: the stream uses the caller's
    // own date format and is in order already.
    let iso = |e: &Entry| {
        let b = e.created_on.as_bytes();
        b.len() > 10 && b[4] == b'-' && b[7] == b'-'
    };
    if streams.len() > 1 && entries.iter().all(iso) {
        entries.sort_by(|a, b| b.created_on.cmp(&a.created_on));
    }
    Ok(entries)
}

/// An entry starts with `TIME - AUTHOR (LABEL)` on its own line.
fn parse_stream(stream: &str, entries: &mut Vec<Entry>) -> Result<()> {
    let start = entries.len();
    for line in stream.lines() {
        if let Some((created_on, author, label)) = header(line) {
            entries.push(Entry {
                created_on: created_on.to_owned(),
                author: author.to_owned(),
                element: element_of(&label.to_lowercase()),
                label: Some(label.to_owned()),
                text: String::new(),
            });
        } else if let Some(entry) = entries[start..].last_mut() {
            if !entry.text.is_empty() {
                entry.text.push('\n');
            }
            entry.text.push_str(line);
        } else if !line.trim().is_empty() {
            return Err(Error::api(
                "the journal stream of this record has a format that is not known; read it as it is with `sn journal ... --raw`",
                json!({ "line": line }),
            ));
        }
    }
    for entry in &mut entries[start..] {
        entry.text.truncate(entry.text.trim_end().len());
    }
    Ok(())
}

/// The time is in the caller's date format, so only its shape is checked: it
/// starts with a digit and has a colon.
fn header(line: &str) -> Option<(&str, &str, &str)> {
    let (time, rest) = line.trim_end().strip_suffix(')')?.split_once(" - ")?;
    if !time.starts_with(|c: char| c.is_ascii_digit()) || !time.contains(':') {
        return None;
    }
    let (author, label) = rest.rsplit_once(" (")?;
    (!author.is_empty() && !label.is_empty()).then_some((time, author, label))
}

/// Labels differ by instance: "Comments", "Additional comments", "Work notes".
fn element_of(label: &str) -> Option<&'static str> {
    if label.contains("work note") || label == WORK_NOTES {
        Some(WORK_NOTES)
    } else if label.contains("comment") {
        Some(COMMENTS)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &str = "2026-08-11 15:40:37 - Ada Lovelace (Work notes)\nline one\n\nline two (really)\n\n2026-08-11 10:00:00 - Bob - Jr (x) (Additional comments)\r\nhello\r\n\r\n";

    #[test]
    fn streams_become_entries() {
        let entries = parse(&[STREAM]).expect("entries");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].author, "Ada Lovelace");
        assert_eq!(entries[0].element, Some("work_notes"));
        assert_eq!(entries[0].text, "line one\n\nline two (really)");
        assert_eq!(entries[1].author, "Bob - Jr (x)");
        assert_eq!(entries[1].element, Some("comments"));
        assert_eq!(entries[1].text, "hello");
    }

    #[test]
    fn two_streams_merge_newest_first_unless_the_dates_do_not_sort() {
        let comments = "2026-08-12 09:00:00 - A (Comments)\nnew\n";
        let entries = parse(&[STREAM, comments]).expect("entries");
        let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["new", "line one\n\nline two (really)", "hello"]);
        let local = [
            "08/11/2026 09:00:00 - A (Comments)\na",
            "09/11/2026 09:00:00 - B (Work notes)\nb",
        ];
        let entries = parse(&local).expect("entries");
        assert_eq!(entries[0].text, "a");
    }

    #[test]
    fn unknown_labels_have_no_element_and_unknown_formats_fail() {
        let entries = parse(&["2026-08-11 15:40:37 - A (Notes de travail)\nx"]).expect("entries");
        assert_eq!(entries[0].element, None);
        assert!(parse(&["not a header\n"]).is_err());
        assert!(parse(&["", "\n"]).expect("empty").is_empty());
    }
}
