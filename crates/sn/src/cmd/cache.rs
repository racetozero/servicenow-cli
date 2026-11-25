//! The offline copy of an instance's tables and columns: one JSON file per
//! instance, for `--offline`, `cache tables`, and shell completion.
//!
//! The file has each table's parent and its own columns; inherited columns
//! are found by walking the parents. The columns come from grouped counts of
//! `sys_dictionary`, which the instance answers about twenty times faster
//! than its rows, in ranges of table names. The ranges follow the order of
//! the database, and the first and last are open, so no name is lost.

use crate::ctx::Ctx;
use clap::Subcommand;
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::config::Store;
use sn_core::{Client, Error, Result, now_unix};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::ops::{Bound, Range};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const FORMAT: u32 = 1;
/// Rows of one range, at about 0.6 ms per row.
const SHARD_ROWS: u64 = 10_000;
const PARALLEL: usize = 4;
const MAX_SPLITS: usize = 4;
/// Rows without the tables of catalog variables, which are no real tables.
const DICTIONARY: &str = "elementISNOTEMPTY^nameNOT LIKEvar__m_";
const TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Subcommand, Debug)]
pub enum CacheCommand {
    /// Read all tables and columns of the instance (columns need the admin role).
    Refresh,
    /// Where the copy is, how old it is, and what it has.
    Status,
    /// Table names from the copy.
    Tables {
        /// Start of the name, for example cmdb_ci_.
        prefix: Option<String>,
    },
    /// Columns of a table from the copy, inherited ones too.
    Columns {
        /// Table name.
        table: String,
    },
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Index {
    format: u32,
    instance: String,
    built_at: u64,
    /// False when the user could not read `sys_dictionary`.
    columns_indexed: bool,
    tables: BTreeMap<String, Table>,
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
struct Table {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    columns: Vec<String>,
}

impl Index {
    pub fn table_names<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = &'a str> {
        self.tables
            .range::<str, _>((Bound::Included(prefix), Bound::Unbounded))
            .map(|(name, _)| name.as_str())
            .take_while(move |name| name.starts_with(prefix))
    }

    /// The columns of a table and its parents, or `None` for an unknown one.
    pub fn columns(&self, table: &str) -> Option<Vec<String>> {
        let mut at = Some(self.tables.get(table)?);
        let mut columns = BTreeSet::new();
        // Bounded: a broken hierarchy can have a cycle.
        for _ in 0..32 {
            let Some(entry) = at else { break };
            columns.extend(entry.columns.iter().cloned());
            at = entry.parent.as_ref().and_then(|p| self.tables.get(p));
        }
        Some(columns.into_iter().collect())
    }
}

/// `https://acme.service-now.com` as a directory name.
fn key(instance: &str) -> String {
    instance
        .split("://")
        .last()
        .unwrap_or(instance)
        .trim_end_matches('/')
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') => c,
            _ => '_',
        })
        .collect()
}

/// The cache directory of an instance (its address or host).
pub fn directory(store: &Store, instance: &str) -> PathBuf {
    store.cache_dir().join(key(instance))
}

fn index_path(store: &Store, instance: &str) -> PathBuf {
    directory(store, instance).join("schema.json")
}

/// The copy of an instance, when there is a usable one. Reads only the disk.
pub fn load(store: &Store, instance: &str) -> Result<Option<Index>> {
    let path = index_path(store, instance);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(Error::invalid(format!(
                "cannot read {} ({err}); check its permissions, or run `sn cache refresh`",
                path.display()
            )));
        }
    };
    match serde_json::from_slice::<Index>(&bytes) {
        Ok(index) if index.format == FORMAT => Ok(Some(index)),
        _ => Err(Error::invalid(format!(
            "{} is damaged or from another version; run `sn cache refresh`",
            path.display()
        ))),
    }
}

/// Replaces a file in the cache as a whole.
pub fn write(path: &Path, content: &[u8]) -> Result<()> {
    let failed = |err: std::io::Error| {
        Error::invalid(format!(
            "cannot write {} ({err}); check the directory, or set SN_CACHE_DIR",
            path.display()
        ))
    };
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(failed)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(failed)?;
    file.write_all(content).map_err(failed)?;
    file.persist(path).map_err(|err| failed(err.error))?;
    Ok(())
}

/// The copy of the selected profile's instance, or how to make one.
pub fn required(ctx: &Ctx) -> Result<Index> {
    let instance = ctx.resolve()?.profile.instance;
    load(&ctx.store, &instance)?.ok_or_else(|| {
        Error::NotFound(format!(
            "there is no offline copy of {instance}; run `sn cache refresh`"
        ))
    })
}

/// The columns of a table, or why the copy has none.
pub fn required_columns(ctx: &Ctx, table: &str) -> Result<Vec<String>> {
    let index = required(ctx)?;
    if !index.columns_indexed {
        return Err(Error::invalid(
            "the offline copy has only table names, because its user could not read sys_dictionary; run `sn cache refresh` with an administrator's profile",
        ));
    }
    index.columns(table).ok_or_else(|| {
        Error::NotFound(format!(
            "the offline copy has no table `{table}`; check the name with `sn cache tables`, or run `sn cache refresh` when it is new"
        ))
    })
}

pub async fn run(ctx: &Ctx, command: CacheCommand) -> Result<()> {
    match command {
        CacheCommand::Refresh => refresh(ctx).await,
        CacheCommand::Status => {
            let instance = ctx.resolve()?.profile.instance;
            let path = index_path(&ctx.store, &instance);
            match load(&ctx.store, &instance)? {
                Some(index) => ctx.out.item(summary(&index, &path)),
                None => ctx.out.item(
                    json!({ "instance": instance, "path": path.display().to_string(), "exists": false }),
                ),
            }
        }
        CacheCommand::Tables { prefix } => {
            let index = required(ctx)?;
            let names = index.table_names(prefix.as_deref().unwrap_or("")).collect();
            lines(ctx, names)
        }
        CacheCommand::Columns { table } => {
            let columns = required_columns(ctx, &table)?;
            lines(ctx, columns.iter().map(String::as_str).collect())
        }
    }
}

/// Names: one per line for people, an array for machines.
fn lines(ctx: &Ctx, names: Vec<&str>) -> Result<()> {
    let text = names.join("\n");
    let meta = json!({ "count": names.len() });
    ctx.out.item_with(json!(names), meta, |_| text)
}

fn summary(index: &Index, path: &Path) -> Value {
    json!({
        "instance": index.instance, "path": path.display().to_string(), "exists": true,
        "built_at": index.built_at, "age_secs": now_unix().saturating_sub(index.built_at),
        "tables": index.tables.len(),
        "columns": index.tables.values().map(|t| t.columns.len()).sum::<usize>(),
        "columns_indexed": index.columns_indexed,
    })
}

async fn refresh(ctx: &Ctx) -> Result<()> {
    let client = ctx.client().await?;
    let timeout = ctx.globals.timeout.is_none().then_some(TIMEOUT);
    let started = Instant::now();
    let (index, requests) = build(client, timeout).await?;
    let path = index_path(&ctx.store, client.base());
    write(&path, &serde_json::to_vec(&index)?)?;
    if !index.columns_indexed {
        ctx.out.warn("your user cannot read sys_dictionary, so the copy has table names only; refresh it with an administrator's profile for columns");
    }
    let mut data = summary(&index, &path);
    data["requests"] = requests.into();
    data["elapsed_ms"] = u64::try_from(started.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .into();
    let text = format!(
        "Saved {} tables and {} columns to {}",
        data["tables"],
        data["columns"],
        path.display()
    );
    ctx.out.done(data, &text)
}

async fn aggregate(
    client: &Client,
    table: &str,
    filter: &str,
    group: &str,
    order: bool,
    timeout: Option<Duration>,
) -> Result<Vec<Value>> {
    let mut request = Request::new(Method::GET, format!("/api/now/stats/{table}"))
        .query("sysparm_group_by", group)
        .query("sysparm_count", "true")
        .query("sysparm_display_value", "false");
    if !filter.is_empty() {
        request = request.query("sysparm_query", filter);
    }
    if order {
        request = request.query("sysparm_order_by", group);
    }
    request.timeout = timeout;
    match client.send(request).await?.result()? {
        Value::Array(rows) => Ok(rows),
        other => Err(Error::api(
            format!("the grouped count of {table} answered in an unknown shape"),
            other,
        )),
    }
}

/// Pairs of a table and one of its columns.
type Columns = Vec<(String, String)>;

/// One group field of a row.
fn group<'a>(row: &'a Value, field: &str) -> Option<&'a str> {
    row["groupby_fields"]
        .as_array()?
        .iter()
        .find(|g| g["field"] == field)?["value"]
        .as_str()
}

async fn build(client: &Client, timeout: Option<Duration>) -> Result<(Index, usize)> {
    let (tables, plan) = tokio::join!(
        aggregate(
            client,
            "sys_db_object",
            "",
            "name,super_class.name",
            false,
            timeout
        ),
        aggregate(client, "sys_dictionary", DICTIONARY, "name", true, timeout),
    );
    let mut index = Index {
        format: FORMAT,
        instance: client.base().to_owned(),
        built_at: now_unix(),
        columns_indexed: true,
        tables: parse_tables(&tables?)?,
    };
    let plan: Vec<(String, u64)> = match plan {
        Ok(rows) => rows.iter().filter_map(plan_entry).collect(),
        // Only administrators read the dictionary; names are worth having.
        Err(err) if err.status() == Some(403) => {
            index.columns_indexed = false;
            return Ok((index, 2));
        }
        Err(err) => return Err(err),
    };
    let mut requests = 2;
    let mut pending = shards(&plan, SHARD_ROWS);
    for _ in 0..=MAX_SPLITS {
        requests += pending.len();
        let fetched: Vec<(Range<usize>, Result<Columns>)> =
            stream::iter(std::mem::take(&mut pending))
                .map(|shard| async {
                    let rows = fetch(client, &plan, &shard, timeout).await;
                    (shard, rows)
                })
                .buffer_unordered(PARALLEL)
                .collect()
                .await;
        for (shard, rows) in fetched {
            match rows {
                Ok(rows) => {
                    for (table, column) in rows {
                        if let Some(entry) = index.tables.get_mut(&table) {
                            entry.columns.push(column);
                        }
                    }
                }
                // Too much at once: halve it and try again.
                Err(err) if too_slow(&err) && shard.len() > 1 => {
                    let middle = shard.start + shard.len() / 2;
                    pending.push(shard.start..middle);
                    pending.push(middle..shard.end);
                }
                Err(err) => return Err(err),
            }
        }
        if pending.is_empty() {
            for table in index.tables.values_mut() {
                table.columns.sort();
                table.columns.dedup();
            }
            return Ok((index, requests));
        }
    }
    Err(Error::Other {
        code: "timeout",
        message:
            "sys_dictionary kept timing out; run `sn cache refresh` again with a larger --timeout"
                .into(),
        data: Value::Null,
    })
}

/// A timeout here, at a gateway, or at the instance, which then cuts its
/// answer off.
fn too_slow(err: &Error) -> bool {
    matches!(err, Error::Network(_))
        || err.code() == "timeout"
        || matches!(err.status(), Some(502..=504))
        || matches!(err, Error::Internal(_))
}

fn parse_tables(rows: &[Value]) -> Result<BTreeMap<String, Table>> {
    let mut tables = BTreeMap::new();
    let mut parents = false;
    for row in rows {
        let Some(name) = group(row, "name").filter(|n| !n.is_empty()) else {
            continue;
        };
        let parent = group(row, "super_class.name");
        parents |= parent.is_some();
        let table = Table {
            parent: parent.filter(|p| !p.is_empty()).map(str::to_owned),
            columns: Vec::new(),
        };
        tables.insert(name.to_owned(), table);
    }
    // Without the parents, every inherited column would be missing.
    match parents {
        true => Ok(tables),
        false => Err(Error::api(
            "the instance did not say which tables extend which; check that you can read sys_db_object",
            Value::Null,
        )),
    }
}

/// A table and how many columns it has.
fn plan_entry(row: &Value) -> Option<(String, u64)> {
    let name = group(row, "name").filter(|n| !n.is_empty())?;
    let count = match &row["stats"]["count"] {
        Value::String(text) => text.parse().unwrap_or(0),
        other => other.as_u64().unwrap_or(0),
    };
    Some((name.to_owned(), count))
}

/// Ranges of the plan with about `target` rows each.
fn shards(plan: &[(String, u64)], target: u64) -> Vec<Range<usize>> {
    let mut shards = Vec::new();
    let mut start = 0;
    let mut rows = 0;
    for (at, (_, count)) in plan.iter().enumerate() {
        if rows > 0 && rows + count > target {
            shards.push(start..at);
            start = at;
            rows = 0;
        }
        rows += count;
    }
    shards.push(start..plan.len());
    shards
}

fn shard_query(plan: &[(String, u64)], shard: &Range<usize>) -> String {
    let mut filter = DICTIONARY.to_owned();
    if shard.start > 0 {
        filter.push_str(&format!("^name>={}", plan[shard.start].0));
    }
    if shard.end < plan.len() {
        filter.push_str(&format!("^name<{}", plan[shard.end].0));
    }
    filter
}

async fn fetch(
    client: &Client,
    plan: &[(String, u64)],
    shard: &Range<usize>,
    timeout: Option<Duration>,
) -> Result<Columns> {
    let filter = shard_query(plan, shard);
    let rows = aggregate(
        client,
        "sys_dictionary",
        &filter,
        "name,element",
        false,
        timeout,
    )
    .await?;
    let found: Columns = rows
        .iter()
        .filter_map(|row| {
            let name = group(row, "name").filter(|n| !n.is_empty())?;
            let element = group(row, "element").filter(|e| !e.is_empty())?;
            Some((name.to_owned(), element.to_owned()))
        })
        .collect();
    // The instance drops a range it cannot evaluate and answers with
    // everything. A few foreign rows are tables created meanwhile.
    let own: HashSet<&str> = plan[shard.clone()]
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    let foreign = found
        .iter()
        .filter(|(name, _)| !own.contains(name.as_str()))
        .count() as u64;
    let planned: u64 = plan[shard.clone()].iter().map(|(_, count)| count).sum();
    match !plan.is_empty() && foreign > planned {
        true => Err(Error::api(
            format!(
                "the instance ignored the range of table names ({foreign} rows outside of it, {planned} expected), so no copy was made"
            ),
            filter.into(),
        )),
        false => Ok(found),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(counts: &[(&str, u64)]) -> Vec<(String, u64)> {
        counts.iter().map(|(n, c)| ((*n).to_owned(), *c)).collect()
    }

    #[test]
    fn ranges_cover_every_name() {
        let plan = plan(&[("a", 6), ("b", 5), ("c", 20), ("d", 1)]);
        let ranges = shards(&plan, 10);
        assert_eq!(ranges, [0..1, 1..2, 2..3, 3..4]);
        let queries: Vec<String> = ranges.iter().map(|r| shard_query(&plan, r)).collect();
        assert_eq!(queries[0], format!("{DICTIONARY}^name<b"));
        assert_eq!(queries[1], format!("{DICTIONARY}^name>=b^name<c"));
        assert_eq!(queries[3], format!("{DICTIONARY}^name>=d"));
        assert_eq!(shards(&[], 10).len(), 1);
    }

    #[test]
    fn columns_include_inherited_ones_and_survive_cycles() {
        let table = |parent: Option<&str>, columns: &[&str]| Table {
            parent: parent.map(str::to_owned),
            columns: columns.iter().map(|c| (*c).to_owned()).collect(),
        };
        let index = Index {
            format: FORMAT,
            instance: "x".into(),
            built_at: 0,
            columns_indexed: true,
            tables: [
                ("task".to_owned(), table(None, &["number"])),
                (
                    "incident".to_owned(),
                    table(Some("task"), &["caller_id", "number"]),
                ),
                ("loop_a".to_owned(), table(Some("loop_b"), &["a"])),
                ("loop_b".to_owned(), table(Some("loop_a"), &["b"])),
            ]
            .into(),
        };
        assert_eq!(
            index.columns("incident").expect("known"),
            ["caller_id", "number"]
        );
        assert_eq!(index.columns("loop_a").expect("known"), ["a", "b"]);
        assert_eq!(index.columns("none"), None);
        assert_eq!(
            index.table_names("loop").collect::<Vec<_>>(),
            ["loop_a", "loop_b"]
        );
        assert_eq!(key("https://ACME.service-now.com/"), "acme.service-now.com");
        assert_eq!(key("http://127.0.0.1:80"), "127.0.0.1_80");
    }
}
