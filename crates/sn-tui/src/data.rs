//! The data layer: runs the state machine's calls against an instance.

use crate::model::{Meta, Row};
use crate::msg::{
    Attachment, Call, Candidate, Change, Entry, EntryKind, Fault, Outcome, Page, Reply, Sla,
    TAB_LIMIT, Write, Written,
};
use crate::tables::{Me, Related};
use crate::text;
use futures_util::future::join_all;
use serde_json::{Map, Value};
use sn_core::client::{Method, Request};
use sn_core::query::{self, term};
use sn_core::{Client, DisplayValue, TableQuery, refs};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::AbortHandle;

/// A transfer may take this long.
const TRANSFER: Duration = Duration::from_secs(24 * 3600);
/// A download that sends nothing for this long has failed.
const IDLE: Duration = Duration::from_secs(60);

pub type Replies = UnboundedSender<Reply>;

/// Where answers come from: an instance, or a fake in tests.
pub trait Backend: Send + Sync + 'static {
    /// Runs one call and sends its answers.
    fn run(&self, call: Call, replies: Replies) -> impl Future<Output = ()> + Send;
}

/// Runs calls beside the interface, and stops those that a newer one
/// replaces.
pub struct Runner<B> {
    backend: Arc<B>,
    replies: Replies,
    running: HashMap<&'static str, AbortHandle>,
}

impl<B: Backend> Runner<B> {
    pub fn new(backend: Arc<B>, replies: Replies) -> Self {
        Self {
            backend,
            replies,
            running: HashMap::new(),
        }
    }

    pub fn start(&mut self, call: Call) {
        let slot = call.slot();
        let (backend, replies) = (self.backend.clone(), self.replies.clone());
        let task = tokio::spawn(async move { backend.run(call, replies).await });
        if let Some(slot) = slot
            && let Some(older) = self.running.insert(slot, task.abort_handle())
        {
            older.abort();
        }
    }
}

/// A ServiceNow instance.
pub struct Live {
    client: Arc<Client>,
    /// Counts run beside the list, a few at a time: an instance answers the
    /// requests of one session one after the other.
    counting: Semaphore,
}

impl Live {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            client,
            counting: Semaphore::new(4),
        }
    }

    async fn me(&self) -> Outcome<Me> {
        let me = refs::me(&self.client).await?;
        let field = |name: &str| sn_core::table::text(&me, name).to_owned();
        let sys_id = field("sys_id");
        let groups = self
            .rows(
                "sys_user_grmember",
                TableQuery::new(term("user", "=", &sys_id))
                    .fields(&["group", "user"])
                    .limit(500),
            )
            .await
            .ok()
            .map(|page| {
                page.rows
                    .iter()
                    .filter(|r| r.value("user") == sys_id)
                    .map(|r| r.value("group").to_owned())
                    .filter(|g| sn_core::is_sys_id(g))
                    .collect()
            });
        Ok(Me {
            name: field("name"),
            sys_id,
            groups,
        })
    }

    async fn rows(&self, table: &str, params: TableQuery) -> Outcome<Page> {
        let params = params.display(DisplayValue::All);
        let page = self.client.list(table, &params).await?;
        Ok(Page {
            rows: page.rows.iter().map(|r| Row::new(table, r)).collect(),
            total: page.total,
        })
    }

    async fn record(&self, table: &str, sys_id: &str, fields: &[&str]) -> Outcome<Row> {
        let params = TableQuery::default()
            .fields(fields)
            .display(DisplayValue::All);
        let row = self.client.record(table, sys_id, &params).await?;
        Ok(Row::new(table, &row))
    }

    async fn meta(&self, table: &str) -> Outcome<Meta> {
        query::check_name("table", table)?;
        let direct = self.client.get(format!("/api/now/ui/meta/{table}")).await;
        if let Ok(result) = &direct
            && let Some(meta) = Meta::from_ui(result).filter(|m| !m.fields.is_empty())
        {
            return Ok(meta);
        }
        if let Err(err) = direct
            && !matches!(err.status(), Some(400..=404))
        {
            return Err(err.into());
        }
        self.dictionary(table).await
    }

    /// The fields from the dictionary, for instances without `ui/meta`.
    async fn dictionary(&self, table: &str) -> Outcome<Meta> {
        let parents = [
            "name",
            "super_class.name",
            "super_class.super_class.name",
            "super_class.super_class.super_class.name",
            "super_class.super_class.super_class.super_class.name",
        ];
        let chain = self
            .rows(
                "sys_db_object",
                TableQuery::new(term("name", "=", table))
                    .fields(&parents)
                    .limit(1),
            )
            .await?;
        let tables: Vec<String> = match chain.rows.iter().find(|r| r.value("name") == table) {
            Some(row) => parents
                .iter()
                .map(|f| row.value(f).to_owned())
                .filter(|t| !t.is_empty())
                .collect(),
            None => vec![table.to_owned()],
        };
        let names = tables.join(",");
        let (dictionary, choices) = tokio::try_join!(
            self.rows(
                "sys_dictionary",
                TableQuery::new(format!("nameIN{names}^elementISNOTEMPTY^active=true"))
                    .fields(&[
                        "name",
                        "element",
                        "column_label",
                        "internal_type",
                        "mandatory",
                        "read_only",
                        "reference",
                        "max_length",
                    ])
                    .limit(2000),
            ),
            self.rows(
                "sys_choice",
                TableQuery::new(format!("nameIN{names}^inactive=false^language=en"))
                    .fields(&["name", "element", "value", "label", "sequence"])
                    .limit(2000),
            ),
        )?;
        Ok(Meta::from_dictionary(
            &tables,
            &dictionary.rows,
            &choices.rows,
        ))
    }

    async fn activity(&self, table: &str, sys_id: &str) -> Outcome<Vec<Entry>> {
        let filter =
            format!("element_id={sys_id}^elementINcomments,work_notes^ORDERBYDESCsys_created_on");
        let journal = self
            .rows(
                "sys_journal_field",
                TableQuery::new(filter)
                    .fields(&[
                        "element",
                        "element_id",
                        "value",
                        "sys_created_on",
                        "sys_created_by",
                    ])
                    .limit(TAB_LIMIT),
            )
            .await;
        let failed = match journal {
            Ok(page) => {
                let entries: Vec<Entry> = page
                    .rows
                    .iter()
                    .filter(|r| r.value("element_id") == sys_id)
                    .map(|r| Entry {
                        kind: match r.value("element") {
                            "work_notes" => EntryKind::WorkNote,
                            _ => EntryKind::Comment,
                        },
                        by: text::line(r.display("sys_created_by")),
                        at: text::line(r.display("sys_created_on")),
                        time: text::parse_utc(r.value("sys_created_on")),
                        text: text::block(&text::html(r.value("value"))),
                    })
                    .collect();
                if !entries.is_empty() {
                    return Ok(entries);
                }
                None
            }
            Err(fault) if fault.code == "permission" || fault.code == "not_found" => Some(fault),
            Err(fault) => return Err(fault),
        };
        // Many users cannot read the journal table, but can read the journal
        // of the record.
        match self
            .record(table, sys_id, &["comments", "work_notes"])
            .await
        {
            Ok(row) => {
                let mut entries = parse_journal(row.display("comments"), EntryKind::Comment);
                entries.extend(parse_journal(
                    row.display("work_notes"),
                    EntryKind::WorkNote,
                ));
                entries.sort_by(|a, b| b.at.cmp(&a.at));
                Ok(entries)
            }
            Err(fault) => Err(failed.unwrap_or(fault)),
        }
    }

    async fn related(&self, list: &Related) -> Outcome<Vec<Row>> {
        let mut fields: Vec<&str> = list.fields.to_vec();
        if !fields.contains(&list.field) {
            fields.push(list.field);
        }
        let page = self
            .rows(
                list.table,
                TableQuery::new(list.query())
                    .fields(&fields)
                    .limit(TAB_LIMIT),
            )
            .await?;
        // The instance drops a term whose field does not exist, and then
        // answers with records that have nothing to do with this one.
        Ok(page
            .rows
            .into_iter()
            .filter(|r| r.value(list.field) == list.value)
            .collect())
    }

    async fn attachments(&self, table: &str, sys_id: &str) -> Outcome<Vec<Attachment>> {
        let filter = format!("table_name={table}^table_sys_id={sys_id}^ORDERBYDESCsys_created_on");
        let request = Request::new(Method::GET, "/api/now/attachment")
            .query("sysparm_query", filter)
            .query("sysparm_limit", TAB_LIMIT.to_string());
        let result = self.client.send(request).await?.result()?;
        let rows = result.as_array().cloned().unwrap_or_default();
        Ok(rows
            .iter()
            .map(|r| Row::new("sys_attachment", r))
            .filter(|r| r.value("table_sys_id") == sys_id)
            .map(|r| Attachment {
                name: text::line(r.value("file_name")),
                size: r.value("size_bytes").parse().unwrap_or_default(),
                by: text::line(r.value("sys_created_by")),
                time: text::parse_utc(r.value("sys_created_on")),
                sys_id: r.sys_id,
            })
            .collect())
    }

    async fn slas(&self, sys_id: &str) -> Outcome<Vec<Sla>> {
        let page = self
            .rows(
                "task_sla",
                TableQuery::new(format!("task={sys_id}^ORDERBYDESCsys_created_on"))
                    .fields(&[
                        "task",
                        "sla",
                        "stage",
                        "has_breached",
                        "percentage",
                        "business_percentage",
                        "time_left",
                        "business_time_left",
                        "planned_end_time",
                    ])
                    .limit(TAB_LIMIT),
            )
            .await?;
        Ok(page
            .rows
            .iter()
            .filter(|r| r.value("task") == sys_id)
            .map(|r| {
                let first = |names: [&str; 2]| {
                    names
                        .into_iter()
                        .map(|n| r.display(n))
                        .find(|t| !t.is_empty())
                        .unwrap_or_default()
                        .to_owned()
                };
                Sla {
                    name: text::line(r.display("sla")),
                    stage: text::line(r.display("stage")),
                    breached: r.value("has_breached") == "true",
                    percent: first(["business_percentage", "percentage"])
                        .replace(',', "")
                        .trim_end_matches('%')
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .map(|p| p.max(0.0) as u32),
                    left: text::line(&first(["business_time_left", "time_left"])),
                    due: text::line(r.display("planned_end_time")),
                    due_time: text::parse_utc(r.value("planned_end_time")),
                }
            })
            .collect())
    }

    async fn history(&self, sys_id: &str) -> Outcome<Vec<Change>> {
        let page = self
            .rows(
                "sys_audit",
                TableQuery::new(format!("documentkey={sys_id}^ORDERBYDESCsys_created_on"))
                    .fields(&[
                        "documentkey",
                        "fieldname",
                        "oldvalue",
                        "newvalue",
                        "user",
                        "sys_created_on",
                    ])
                    .limit(TAB_LIMIT),
            )
            .await?;
        Ok(page
            .rows
            .iter()
            .filter(|r| r.value("documentkey") == sys_id)
            .map(|r| {
                let field = r.value("fieldname").to_owned();
                // The secret is in the old and new value, not in the name of
                // the audit field.
                let hide = |text: &str| match crate::model::is_sensitive(&field) && !text.is_empty()
                {
                    true => crate::model::REDACTED.to_owned(),
                    false => text::line(text),
                };
                Change {
                    old: hide(r.value("oldvalue")),
                    new: hide(r.value("newvalue")),
                    by: text::line(r.display("user")),
                    time: text::parse_utc(r.value("sys_created_on")),
                    field,
                }
            })
            .collect())
    }

    async fn write(&self, write: &Write) -> Written {
        if let Some(baseline) = &write.baseline {
            let mut fields: Vec<&str> = vec![
                "sys_id",
                "number",
                "sys_updated_on",
                "sys_updated_by",
                "sys_mod_count",
            ];
            fields.extend(write.body.keys().map(String::as_str));
            match self.record(&write.table, &write.sys_id, &fields).await {
                Ok(now) if baseline.conflicts(&now.baseline()) => return Written::Conflict(now),
                Ok(_) => {}
                Err(fault) => return Written::Failed(fault),
            }
        }
        let params = TableQuery::default().display(DisplayValue::All);
        match self
            .client
            .update(&write.table, &write.sys_id, write.body.clone(), &params)
            .await
        {
            Ok(value) => {
                let row = Row::new(&write.table, &value);
                Written::Saved {
                    kept: not_taken(&write.body, &row),
                    row,
                }
            }
            Err(err) => Written::Failed(err.into()),
        }
    }

    async fn download(&self, sys_id: &str, name: &str, dir: &Path) -> Outcome<PathBuf> {
        let request = Request::new(Method::GET, format!("/api/now/attachment/{sys_id}/file"))
            .timeout(TRANSFER);
        let stalled = |_| Fault {
            code: "timeout",
            message: format!(
                "you downloaded {name} and the instance sent nothing for {} seconds; check the connection, then press Enter on the file again",
                IDLE.as_secs()
            ),
        };
        // The client may try four times before it has an answer.
        let mut response = tokio::time::timeout(IDLE * 4, self.client.open(request))
            .await
            .map_err(stalled)??;
        let (path, mut file) = new_file(dir, name).await?;
        let problem = |err: std::io::Error| Fault {
            code: "invalid",
            message: format!(
                "cannot write {} ({err}); check the free space and your access to the directory",
                path.display()
            ),
        };
        let copied = async {
            while let Some(chunk) = tokio::time::timeout(IDLE, response.chunk())
                .await
                .map_err(stalled)?
                .map_err(|e| Fault::from(sn_core::Error::from(e)))?
            {
                file.write_all(&chunk).await.map_err(problem)?;
            }
            file.flush().await.map_err(problem)
        }
        .await;
        if let Err(fault) = copied {
            // Half a file would look like the whole one.
            let _ = tokio::fs::remove_file(&path).await;
            return Err(fault);
        }
        Ok(path)
    }

    async fn references(&self, table: &str, text: &str) -> Outcome<Vec<Candidate>> {
        let text = text.trim();
        let like = |fields: &[&str]| match text.is_empty() {
            true => String::new(),
            false => fields
                .iter()
                .map(|f| term(f, "LIKE", text))
                .collect::<Vec<_>>()
                .join("^OR"),
        };
        let (filter, fields): (String, &[&str]) = match table {
            "sys_user" => (
                query::all([
                    "active=true",
                    &like(&["name", "user_name", "email"]),
                    "ORDERBYname",
                ]),
                &["name", "user_name", "email", "title"],
            ),
            "sys_user_group" => (
                query::all(["active=true", &like(&["name"]), "ORDERBYname"]),
                &["name", "description"],
            ),
            _ if crate::tables::is_task(table) => (
                query::all([
                    like(&["number", "short_description"]).as_str(),
                    "ORDERBYDESCsys_updated_on",
                ]),
                &["number", "short_description"],
            ),
            _ => (
                like(&["name", "number", "title", "short_description"]),
                &["name", "number", "title", "short_description"],
            ),
        };
        let mut wanted = vec!["sys_id"];
        wanted.extend(fields);
        let page = self
            .rows(table, TableQuery::new(filter).fields(&wanted).limit(20))
            .await?;
        let needle = text.to_lowercase();
        Ok(page
            .rows
            .iter()
            .map(|r| {
                let mut words = fields
                    .iter()
                    .map(|f| text::line(r.display(f)))
                    .filter(|t| !t.is_empty());
                Candidate {
                    value: r.sys_id.clone(),
                    label: words.next().unwrap_or_else(|| r.sys_id.clone()),
                    detail: words.collect::<Vec<_>>().join("  "),
                }
            })
            // Terms on fields the table lacks are dropped, so check.
            .filter(|c| {
                needle.is_empty()
                    || c.label.to_lowercase().contains(&needle)
                    || c.detail.to_lowercase().contains(&needle)
            })
            .collect())
    }

    async fn tables(&self, text: &str) -> Outcome<Vec<(String, String)>> {
        let filter = [term("label", "LIKE", text), term("name", "LIKE", text)].join("^OR");
        let page = self
            .rows(
                "sys_db_object",
                TableQuery::new(format!("{filter}^ORDERBYlabel"))
                    .fields(&["name", "label"])
                    .limit(12),
            )
            .await?;
        Ok(page
            .rows
            .iter()
            .filter(|r| !r.value("name").is_empty())
            .map(|r| (r.value("name").to_owned(), text::line(r.display("label"))))
            .collect())
    }

    async fn tasks(&self, text: &str) -> Outcome<Vec<Row>> {
        let filter = query::all([
            term("123TEXTQUERY321", "=", text).as_str(),
            "ORDERBYDESCsys_updated_on",
        ]);
        let page = self
            .rows(
                "task",
                TableQuery::new(filter)
                    .fields(&[
                        "sys_id",
                        "sys_class_name",
                        "number",
                        "short_description",
                        "state",
                    ])
                    .limit(10),
            )
            .await?;
        Ok(page.rows)
    }

    async fn locate(&self, text: &str) -> Outcome<(String, String)> {
        let reference = refs::parse(&self.client, text)?;
        let found = refs::locate(&self.client, reference).await?;
        Ok((found.table, found.sys_id))
    }
}

/// Fields whose value after the write is not the value sent. The instance
/// answers 200 when an access rule or a business rule keeps the old value.
fn not_taken(body: &Map<String, Value>, row: &Row) -> Vec<String> {
    let same = |sent: &str, kept: &str| {
        sent == kept
            || sent.trim() == kept.trim()
            || text::parse_utc(sent).is_some_and(|t| Some(t) == text::parse_utc(kept))
            || matches!((sent.parse::<f64>(), kept.parse::<f64>()), (Ok(a), Ok(b)) if a == b)
            || sent.replace("\r\n", "\n").trim() == kept.replace("\r\n", "\n").trim()
    };
    body.iter()
        .filter(|(field, _)| {
            !matches!(
                field.as_str(),
                "comments" | "work_notes" | "comments_and_work_notes"
            )
        })
        .filter(|(field, _)| row.has(field))
        .filter(|(field, value)| !same(value.as_str().unwrap_or_default(), row.value(field)))
        .map(|(field, _)| field.clone())
        .collect()
}

/// The entries of a journal as a record shows it:
/// `2026-11-20 10:00:00 - Ada Lovelace (Work notes)`, then the text.
fn parse_journal(journal: &str, kind: EntryKind) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for line in journal.lines() {
        let head = line.split_once(" - ").and_then(|(at, rest)| {
            let by = rest.trim_end().strip_suffix(')')?.rsplit_once(" (")?.0;
            (at.len() >= 8 && at.starts_with(|c: char| c.is_ascii_digit()) && at.contains(':'))
                .then_some((at, by))
        });
        match head {
            Some((at, by)) => out.push(Entry {
                kind,
                by: text::line(by),
                at: text::line(at),
                // This time is in the user's own time zone.
                time: None,
                text: String::new(),
            }),
            None => {
                if let Some(entry) = out.last_mut() {
                    entry.text.push_str(line);
                    entry.text.push('\n');
                }
            }
        }
    }
    for entry in &mut out {
        entry.text = text::block(&text::html(&entry.text));
    }
    out
}

/// A new file in `dir`. Never an existing one: `name (1).ext` instead.
async fn new_file(dir: &Path, name: &str) -> Outcome<(PathBuf, tokio::fs::File)> {
    let name: String = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control() && !"<>:\"|?*".contains(*c))
        .collect();
    let name = match name.trim_start_matches('.').trim() {
        "" => "attachment",
        name => name,
    };
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    for n in 0..1000 {
        let path = dir.join(match n {
            0 => name.to_owned(),
            n => format!("{stem} ({n}){extension}"),
        });
        let opened = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await;
        match opened {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => {
                return Err(Fault {
                    code: "invalid",
                    message: format!(
                        "cannot create {} ({err}); start the workspace in a directory you can write to",
                        path.display()
                    ),
                });
            }
        }
    }
    Err(Fault {
        code: "invalid",
        message: format!(
            "{} has too many copies of {name}; remove some first",
            dir.display()
        ),
    })
}

impl Backend for Live {
    async fn run(&self, call: Call, replies: Replies) {
        let reply = match call {
            Call::Me { req } => Reply::Me(req, self.me().await),
            Call::Count { id, table, query } => {
                let _turn = self.counting.acquire().await;
                let count = self.client.count(&table, &query).await;
                Reply::Count(id, count.map_err(Fault::from))
            }
            Call::Meta { table } => {
                let meta = self.meta(&table).await;
                Reply::Meta(table, meta)
            }
            Call::List {
                req,
                table,
                query,
                fields,
                view,
                offset,
                limit,
            } => {
                let params = TableQuery {
                    query,
                    fields,
                    view,
                    limit: Some(limit),
                    offset: (offset > 0).then_some(offset),
                    ..TableQuery::default()
                };
                Reply::List(req, self.rows(&table, params).await)
            }
            Call::Record { req, table, sys_id } => {
                Reply::Record(req, self.record(&table, &sys_id, &[]).await)
            }
            Call::Activity { req, table, sys_id } => {
                Reply::Activity(req, self.activity(&table, &sys_id).await)
            }
            Call::Related { req, lists } => {
                let asks = lists.iter().enumerate().map(|(i, list)| {
                    let replies = replies.clone();
                    async move {
                        let rows = self.related(list).await;
                        let _ = replies.send(Reply::Related(req, i, rows));
                    }
                });
                join_all(asks).await;
                return;
            }
            Call::Attachments { req, table, sys_id } => {
                Reply::Attachments(req, self.attachments(&table, &sys_id).await)
            }
            Call::Slas { req, sys_id } => Reply::Slas(req, self.slas(&sys_id).await),
            Call::History { req, sys_id } => Reply::History(req, self.history(&sys_id).await),
            Call::Write { req, writes } => {
                let mut results = Vec::with_capacity(writes.len());
                for chunk in writes.chunks(4) {
                    results.extend(join_all(chunk.iter().map(|w| self.write(w))).await);
                    let _ = replies.send(Reply::Progress(req, results.len()));
                }
                Reply::Written(req, results)
            }
            Call::Create { req, table, body } => {
                let params = TableQuery::default().display(DisplayValue::All);
                let created = self.client.create(&table, body, &params).await;
                Reply::Created(
                    req,
                    created.map(|v| Row::new(&table, &v)).map_err(Fault::from),
                )
            }
            Call::Download {
                req,
                sys_id,
                name,
                dir,
            } => Reply::Downloaded(req, self.download(&sys_id, &name, &dir).await),
            Call::References { req, table, text } => {
                Reply::References(req, self.references(&table, &text).await)
            }
            Call::Search { req, text, records } => {
                let tables = async {
                    let found = self.tables(&text).await;
                    let _ = replies.send(Reply::Tables(req, found));
                };
                let tasks = async {
                    if records {
                        let found = self.tasks(&text).await;
                        let _ = replies.send(Reply::Records(req, found));
                    }
                };
                tokio::join!(tables, tasks);
                return;
            }
            Call::Locate { req, text } => Reply::Located(req, self.locate(&text).await),
        };
        // The interface is gone when nobody receives.
        let _ = replies.send(reply);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sn_core::client::Options;
    use sn_core::config::{AuthKind, Profile, Resolved, Secrets};
    use tokio::sync::mpsc::unbounded_channel;
    use wiremock::matchers::{body_json, method, path, query_param, query_param_contains};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn live(server: &MockServer, options: Options) -> Live {
        live_at(&server.uri(), options)
    }

    fn live_at(instance: &str, options: Options) -> Live {
        let resolved = Resolved {
            name: "t".into(),
            profile: Profile {
                instance: instance.to_owned(),
                auth: AuthKind::Basic,
                username: Some("ada".into()),
                ..Profile::default()
            },
            secrets: Secrets {
                password: Some("pw".into()),
                ..Secrets::default()
            },
            ephemeral: true,
        };
        Live::new(Arc::new(
            Client::new(resolved, None, options).expect("client"),
        ))
    }

    async fn ask(live: &Live, call: Call) -> Vec<Reply> {
        let (tx, mut rx) = unbounded_channel();
        live.run(call, tx).await;
        let mut out = Vec::new();
        while let Ok(reply) = rx.try_recv() {
            out.push(reply);
        }
        out
    }

    fn result(value: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({ "result": value }))
    }

    const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn write(body: Value, updated: &str) -> Write {
        Write {
            table: "incident".into(),
            sys_id: ID.into(),
            name: "INC1".into(),
            body: body.as_object().cloned().unwrap_or_default(),
            baseline: Some(crate::model::Baseline {
                updated: updated.into(),
                mods: "3".into(),
            }),
        }
    }

    #[tokio::test]
    async fn a_list_asks_for_both_values_and_the_total() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/task"))
            .and(query_param("sysparm_query", "active=true^ORDERBYpriority"))
            .and(query_param("sysparm_display_value", "all"))
            .and(query_param("sysparm_fields", "number,state"))
            .and(query_param("sysparm_limit", "20"))
            .and(query_param("sysparm_offset", "40"))
            .respond_with(
                result(
                    json!([{ "number": {"value": "INC1", "display_value": "INC1"},
                    "state": {"value": "2", "display_value": "In Progress"} }]),
                )
                .insert_header("x-total-count", "41"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let replies = ask(
            &live(&server, Options::default()),
            Call::List {
                req: 7,
                table: "task".into(),
                query: "active=true^ORDERBYpriority".into(),
                fields: vec!["number".into(), "state".into()],
                view: None,
                offset: 40,
                limit: 20,
            },
        )
        .await;
        let [Reply::List(7, Ok(page))] = replies.as_slice() else {
            panic!("{replies:?}");
        };
        assert_eq!(page.total, Some(41));
        assert_eq!(page.rows[0].display("state"), "In Progress");
    }

    #[tokio::test]
    async fn a_write_checks_the_version_and_sends_one_patch() {
        let server = MockServer::start().await;
        let record = json!({ "sys_id": ID, "sys_updated_on": "2026-11-25 08:00:00", "sys_mod_count": "3", "state": "1" });
        Mock::given(method("GET"))
            .and(path(format!("/api/now/table/incident/{ID}")))
            .respond_with(result(record.clone()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!("/api/now/table/incident/{ID}")))
            .and(body_json(
                json!({ "state": "2", "urgency": "1", "work_notes": "hi" }),
            ))
            .respond_with(result(json!({
                "sys_id": ID, "sys_updated_on": "2026-11-25 09:00:00", "sys_mod_count": "4",
                "state": {"value": "2", "display_value": "In Progress"},
                "urgency": {"value": "3", "display_value": "Low"}, "work_notes": "",
            })))
            .expect(1)
            .mount(&server)
            .await;
        let replies = ask(
            &live(&server, Options::default()),
            Call::Write {
                req: 1,
                writes: vec![write(
                    json!({ "state": "2", "urgency": "1", "work_notes": "hi" }),
                    "2026-11-25 08:00:00",
                )],
            },
        )
        .await;
        let Some(Reply::Written(1, results)) = replies.last() else {
            panic!("{replies:?}");
        };
        let Written::Saved { row, kept } = &results[0] else {
            panic!("{results:?}");
        };
        assert_eq!(row.display("state"), "In Progress");
        assert_eq!(kept, &["urgency"], "the instance kept the old urgency");
    }

    #[tokio::test]
    async fn a_conflict_writes_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(result(json!({
                "sys_id": ID, "sys_updated_on": "2026-11-25 08:30:00", "sys_updated_by": "bob",
                "sys_mod_count": "4", "state": "6",
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .respond_with(result(json!({})))
            .expect(0)
            .mount(&server)
            .await;
        let replies = ask(
            &live(&server, Options::default()),
            Call::Write {
                req: 1,
                writes: vec![write(json!({ "state": "2" }), "2026-11-25 08:00:00")],
            },
        )
        .await;
        let Some(Reply::Written(_, results)) = replies.last() else {
            panic!("{replies:?}");
        };
        assert!(
            matches!(&results[0], Written::Conflict(row) if row.value("sys_updated_by") == "bob")
        );
    }

    #[tokio::test]
    async fn dry_run_and_read_only_send_no_write() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .respond_with(result(json!({})))
            .expect(0)
            .mount(&server)
            .await;
        let mut note = write(json!({ "work_notes": "x" }), "");
        note.baseline = None;
        for (options, code) in [
            (
                Options {
                    dry_run: true,
                    ..Options::default()
                },
                "dry_run",
            ),
            (
                Options {
                    read_only: true,
                    ..Options::default()
                },
                "read_only",
            ),
        ] {
            let replies = ask(
                &live(&server, options),
                Call::Write {
                    req: 1,
                    writes: vec![note.clone()],
                },
            )
            .await;
            let Some(Reply::Written(_, results)) = replies.last() else {
                panic!("{replies:?}");
            };
            assert!(
                matches!(&results[0], Written::Failed(fault) if fault.code == code),
                "{results:?}"
            );
        }
    }

    #[tokio::test]
    async fn bulk_results_keep_their_order() {
        let server = MockServer::start().await;
        for n in 0..6u32 {
            let template = match n {
                3 => ResponseTemplate::new(403).set_body_json(
                    json!({"error": {"message": "Operation Failed", "detail": "ACL"}}),
                ),
                _ => result(json!({ "sys_id": n.to_string(), "state": "approved" })),
            };
            Mock::given(method("PATCH"))
                .and(path(format!("/api/now/table/sysapproval_approver/{n}")))
                .respond_with(template)
                .mount(&server)
                .await;
        }
        let writes = (0..6u32)
            .map(|n| Write {
                table: "sysapproval_approver".into(),
                sys_id: n.to_string(),
                name: n.to_string(),
                body: json!({"state": "approved"})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                baseline: None,
            })
            .collect();
        let replies = ask(
            &live(&server, Options::default()),
            Call::Write { req: 1, writes },
        )
        .await;
        let Some(Reply::Written(_, results)) = replies.last() else {
            panic!("{replies:?}");
        };
        assert_eq!(results.len(), 6);
        for (n, result) in results.iter().enumerate() {
            match result {
                Written::Failed(fault) => {
                    assert_eq!(n, 3);
                    assert!(fault.message.contains("ACL"), "{}", fault.message);
                }
                Written::Saved { row, .. } => assert_eq!(row.sys_id, n.to_string()),
                Written::Conflict(_) => panic!("no conflict expected"),
            }
        }
        assert!(replies.contains(&Reply::Progress(1, 4)));
    }

    #[tokio::test]
    async fn activity_falls_back_to_the_journal_of_the_record() {
        let server = MockServer::start().await;
        Mock::given(path("/api/now/table/sys_journal_field"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(json!({"error": {"message": "no"}})),
            )
            .mount(&server)
            .await;
        Mock::given(path(format!("/api/now/table/incident/{ID}")))
            .respond_with(result(json!({
                "comments": {"value": "", "display_value": "2026-11-20 10:00:00 - Bob Stone (Additional comments)\nIs it fixed?\n\n"},
                "work_notes": {"value": "", "display_value": "2026-11-21 09:00:00 - Ada (Work notes)\nline one\nline - two (x)\n\n2026-11-19 09:00:00 - Ada (Work notes)\nfirst"},
            })))
            .mount(&server)
            .await;
        let replies = ask(
            &live(&server, Options::default()),
            Call::Activity {
                req: 1,
                table: "incident".into(),
                sys_id: ID.into(),
            },
        )
        .await;
        let [Reply::Activity(1, Ok(entries))] = replies.as_slice() else {
            panic!("{replies:?}");
        };
        let shown: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| (e.by.as_str(), e.text.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("Ada", "line one\nline - two (x)"),
                ("Bob Stone", "Is it fixed?"),
                ("Ada", "first")
            ]
        );
        assert_eq!(entries[1].kind, EntryKind::Comment);
    }

    #[tokio::test]
    async fn rows_of_other_records_are_dropped() {
        let server = MockServer::start().await;
        Mock::given(path("/api/now/table/task_ci"))
            .respond_with(result(json!([
                {"sys_id": "1", "task": ID, "ci_item": {"value": OTHER, "display_value": "db1"}},
                {"sys_id": "2", "task": OTHER},
            ])))
            .mount(&server)
            .await;
        Mock::given(path("/api/now/table/task_sla"))
            .respond_with(result(json!([
                {"task": {"value": ID, "display_value": "INC1"}, "sla": "P1", "has_breached": "true", "business_percentage": "1,204.5"},
                {"task": OTHER, "sla": "other"},
            ])))
            .mount(&server)
            .await;
        Mock::given(path("/api/now/table/sys_audit"))
            .respond_with(result(json!([
                {"documentkey": ID, "fieldname": "u_api_key", "oldvalue": "old", "newvalue": "new"},
                {"documentkey": ID, "fieldname": "state", "oldvalue": "1", "newvalue": "2"},
                {"documentkey": OTHER, "fieldname": "state"},
            ])))
            .mount(&server)
            .await;
        Mock::given(path("/api/now/attachment"))
            .and(query_param_contains("sysparm_query", "table_name=incident"))
            .respond_with(result(json!([
                {"sys_id": "f", "file_name": "a\u{1b}[2Jb.txt", "size_bytes": "12", "table_sys_id": ID},
                {"sys_id": "g", "file_name": "other", "table_sys_id": OTHER},
            ])))
            .mount(&server)
            .await;
        let live = live(&server, Options::default());
        let lists = vec![Related {
            title: "Affected CIs",
            table: "task_ci",
            field: "task",
            value: ID.into(),
            fields: &["sys_id", "ci_item"],
            link: Some(("cmdb_ci", "ci_item")),
        }];
        let replies = ask(&live, Call::Related { req: 1, lists }).await;
        assert!(matches!(replies.as_slice(), [Reply::Related(1, 0, Ok(rows))] if rows.len() == 1));
        let replies = ask(
            &live,
            Call::Slas {
                req: 2,
                sys_id: ID.into(),
            },
        )
        .await;
        let [Reply::Slas(2, Ok(slas))] = replies.as_slice() else {
            panic!("{replies:?}");
        };
        assert_eq!((slas.len(), slas[0].percent), (1, Some(1204)));
        let replies = ask(
            &live,
            Call::History {
                req: 3,
                sys_id: ID.into(),
            },
        )
        .await;
        let [Reply::History(3, Ok(changes))] = replies.as_slice() else {
            panic!("{replies:?}");
        };
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].new, crate::model::REDACTED);
        let replies = ask(
            &live,
            Call::Attachments {
                req: 4,
                table: "incident".into(),
                sys_id: ID.into(),
            },
        )
        .await;
        let [Reply::Attachments(4, Ok(files))] = replies.as_slice() else {
            panic!("{replies:?}");
        };
        assert_eq!((files.len(), files[0].name.as_str()), (1, "a[2Jb.txt"));
    }

    #[tokio::test]
    async fn a_download_never_replaces_a_file_or_leaves_the_directory() {
        let server = MockServer::start().await;
        Mock::given(path("/api/now/attachment/f/file"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().expect("dir");
        let live = live(&server, Options::default());
        for expected in ["evil.txt", "evil (1).txt"] {
            let replies = ask(
                &live,
                Call::Download {
                    req: 1,
                    sys_id: "f".into(),
                    name: "../../evil.txt".into(),
                    dir: dir.path().into(),
                },
            )
            .await;
            let [Reply::Downloaded(1, Ok(at))] = replies.as_slice() else {
                panic!("{replies:?}");
            };
            assert_eq!(at, &dir.path().join(expected));
            assert_eq!(std::fs::read(at).expect("file"), b"data");
        }
    }

    /// Sends the file in three parts, so the whole takes longer than the
    /// limit of one request.
    #[tokio::test]
    async fn a_slow_download_may_take_longer_than_a_request() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("port");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let _ = socket.read(&mut [0; 4096]).await;
            let head = "HTTP/1.1 200 OK\r\ncontent-length: 9\r\n\r\n";
            for part in [head, "one", "two", "end"] {
                socket.write_all(part.as_bytes()).await.expect("sent");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
        let options = Options {
            timeout: Some(Duration::from_millis(350)),
            ..Options::default()
        };
        let dir = tempfile::tempdir().expect("dir");
        let live = live_at(&format!("http://{address}"), options);
        let replies = ask(
            &live,
            Call::Download {
                req: 1,
                sys_id: "f".into(),
                name: "log".into(),
                dir: dir.path().into(),
            },
        )
        .await;
        let [Reply::Downloaded(1, Ok(at))] = replies.as_slice() else {
            panic!("{replies:?}");
        };
        assert_eq!(std::fs::read(at).expect("file"), b"onetwoend");
    }

    #[tokio::test]
    async fn fields_come_from_the_dictionary_without_ui_meta() {
        let server = MockServer::start().await;
        Mock::given(path("/api/now/ui/meta/incident"))
            .respond_with(
                ResponseTemplate::new(404).set_body_json(json!({"error": {"message": "no"}})),
            )
            .mount(&server)
            .await;
        Mock::given(path("/api/now/table/sys_db_object"))
            .respond_with(result(
                json!([{ "name": "incident", "super_class.name": "task" }]),
            ))
            .mount(&server)
            .await;
        Mock::given(path("/api/now/table/sys_dictionary"))
            .and(query_param_contains("sysparm_query", "nameINincident,task"))
            .respond_with(result(json!([
                {"name": "task", "element": "state", "column_label": "State", "internal_type": "integer"},
                {"name": "incident", "element": "caller_id", "column_label": "Caller", "internal_type": "reference", "reference": "sys_user"},
            ])))
            .mount(&server)
            .await;
        Mock::given(path("/api/now/table/sys_choice"))
            .respond_with(result(
                json!([{ "name": "incident", "element": "state", "value": "1", "label": "New" }]),
            ))
            .mount(&server)
            .await;
        let replies = ask(
            &live(&server, Options::default()),
            Call::Meta {
                table: "incident".into(),
            },
        )
        .await;
        let [Reply::Meta(_, Ok(meta))] = replies.as_slice() else {
            panic!("{replies:?}");
        };
        assert_eq!(
            meta.field("caller_id").and_then(|f| f.reference.as_deref()),
            Some("sys_user")
        );
        assert_eq!(
            meta.field("state").map(|f| f.kind),
            Some(crate::model::Kind::Choice)
        );
    }

    #[tokio::test]
    async fn the_palette_search_reports_tables_and_tasks_on_their_own() {
        let server = MockServer::start().await;
        Mock::given(path("/api/now/table/sys_db_object"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(json!({"error": {"message": "no"}})),
            )
            .mount(&server)
            .await;
        Mock::given(path("/api/now/table/task"))
            .and(query_param_contains("sysparm_query", "123TEXTQUERY321=vpn"))
            .respond_with(result(
                json!([{ "sys_id": ID, "number": "INC1", "sys_class_name": "incident" }]),
            ))
            .mount(&server)
            .await;
        let replies = ask(
            &live(&server, Options::default()),
            Call::Search {
                req: 9,
                text: "vpn".into(),
                records: true,
            },
        )
        .await;
        assert!(
            replies
                .iter()
                .any(|r| matches!(r, Reply::Tables(9, Err(_))))
        );
        assert!(
            replies
                .iter()
                .any(|r| matches!(r, Reply::Records(9, Ok(rows)) if rows[0].table == "incident"))
        );
    }

    #[tokio::test]
    async fn references_that_do_not_match_are_dropped() {
        let server = MockServer::start().await;
        Mock::given(path("/api/now/table/sys_user"))
            .and(query_param(
                "sysparm_query",
                "active=true^nameLIKEad^ORuser_nameLIKEad^ORemailLIKEad^ORDERBYname",
            ))
            .respond_with(result(json!([
                {"sys_id": ID, "name": "Ada Lovelace", "user_name": "ada", "email": "ada@example.com"},
                {"sys_id": OTHER, "name": "Bob", "user_name": "bob"},
            ])))
            .mount(&server)
            .await;
        let replies = ask(
            &live(&server, Options::default()),
            Call::References {
                req: 1,
                table: "sys_user".into(),
                text: " ad ".into(),
            },
        )
        .await;
        let [Reply::References(1, Ok(found))] = replies.as_slice() else {
            panic!("{replies:?}");
        };
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].detail, "ada  ada@example.com");
    }
}
