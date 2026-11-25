//! The Table and Aggregate APIs: reading, paging, writing records.

use crate::client::{Client, Method, Request};
use crate::error::{Error, Result};
use crate::query;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayValue {
    /// Labels.
    #[default]
    True,
    /// Raw values and sys_ids.
    False,
    /// Both, as `{display_value, value}`.
    All,
}

impl DisplayValue {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::True => "true",
            Self::False => "false",
            Self::All => "all",
        }
    }
}

/// `sysparm_*` parameters of a read.
#[derive(Debug, Clone, Default)]
pub struct TableQuery {
    pub query: String,
    pub fields: Vec<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub display: DisplayValue,
    pub view: Option<String>,
    pub query_category: Option<String>,
    pub query_no_domain: bool,
    pub no_count: bool,
    /// Keep the `link` of reference fields.
    pub reference_links: bool,
}

impl TableQuery {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            ..Self::default()
        }
    }

    pub fn fields(mut self, fields: &[&str]) -> Self {
        self.fields = fields.iter().map(|f| (*f).to_owned()).collect();
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn display(mut self, display: DisplayValue) -> Self {
        self.display = display;
        self
    }

    /// Adds the parameters to a request.
    pub fn apply(&self, mut request: Request) -> Request {
        if !self.query.is_empty() {
            request = request.query("sysparm_query", self.query.clone());
        }
        if !self.fields.is_empty() {
            request = request.query("sysparm_fields", self.fields.join(","));
        }
        if let Some(limit) = self.limit {
            request = request.query("sysparm_limit", limit.to_string());
        }
        if let Some(offset) = self.offset {
            request = request.query("sysparm_offset", offset.to_string());
        }
        request = request
            .query("sysparm_display_value", self.display.as_str())
            .query_opt("sysparm_view", self.view.clone())
            .query_opt("sysparm_query_category", self.query_category.clone());
        if !self.reference_links {
            request = request.query("sysparm_exclude_reference_link", "true");
        }
        if self.query_no_domain {
            request = request.query("sysparm_query_no_domain", "true");
        }
        if self.no_count {
            request = request.query("sysparm_no_count", "true");
        }
        request
    }
}

#[derive(Debug, Clone, Default)]
pub struct Page {
    pub rows: Vec<Value>,
    /// Matching records on the server, when it counted them.
    pub total: Option<u64>,
}

impl Page {
    /// The records in an answer of the Table API.
    pub fn read(response: Result<crate::Response>) -> Result<Self> {
        match response {
            Ok(response) => {
                let total = response.total();
                let rows = match response.result()? {
                    Value::Array(rows) => rows,
                    Value::Null => Vec::new(),
                    other => vec![other],
                };
                Ok(Self { rows, total })
            }
            // The Table API answers 404 "No Record found" to an empty match.
            Err(Error::Api(failure))
                if failure.status == Some(404) && failure.message.contains("No Record found") =>
            {
                Ok(Self {
                    rows: Vec::new(),
                    total: Some(0),
                })
            }
            Err(err) => Err(err),
        }
    }
}

pub fn path(table: &str) -> String {
    format!("/api/now/table/{table}")
}

impl Client {
    pub async fn list(&self, table: &str, params: &TableQuery) -> Result<Page> {
        query::check_name("table", table)?;
        let response = self
            .send(params.apply(Request::new(Method::GET, path(table))))
            .await;
        Page::read(response)
    }

    pub async fn record(&self, table: &str, sys_id: &str, params: &TableQuery) -> Result<Value> {
        query::check_name("table", table)?;
        let request = Request::new(Method::GET, format!("{}/{sys_id}", path(table)));
        self.send(params.apply(request)).await?.result()
    }

    pub async fn create(
        &self,
        table: &str,
        body: Map<String, Value>,
        params: &TableQuery,
    ) -> Result<Value> {
        query::check_name("table", table)?;
        let request = Request::new(Method::POST, path(table)).json(body.into());
        self.send(params.apply(request)).await?.result()
    }

    pub async fn update(
        &self,
        table: &str,
        sys_id: &str,
        body: Map<String, Value>,
        params: &TableQuery,
    ) -> Result<Value> {
        query::check_name("table", table)?;
        let request =
            Request::new(Method::PATCH, format!("{}/{sys_id}", path(table))).json(body.into());
        self.send(params.apply(request)).await?.result()
    }

    pub async fn remove(&self, table: &str, sys_id: &str) -> Result<()> {
        query::check_name("table", table)?;
        self.delete(format!("{}/{sys_id}", path(table)))
            .await
            .map(|_| ())
    }

    /// How many records match.
    pub async fn count(&self, table: &str, filter: &str) -> Result<u64> {
        query::check_name("table", table)?;
        let mut request = Request::new(Method::GET, format!("/api/now/stats/{table}"))
            .query("sysparm_count", "true");
        if !filter.is_empty() {
            request = request.query("sysparm_query", filter);
        }
        let result = self.send(request).await?.result()?;
        result
            .pointer("/stats/count")
            .and_then(|c| c.as_u64().or_else(|| c.as_str()?.parse().ok()))
            .ok_or_else(|| Error::api("the instance answered without a count", result.clone()))
    }
}

/// The text of a field in any display mode.
pub fn text<'a>(row: &'a Value, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::String(text)) => text,
        Some(Value::Object(map)) => map
            .get("display_value")
            .or_else(|| map.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        _ => "",
    }
}

/// The raw value of a field in any display mode.
pub fn raw<'a>(row: &'a Value, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::String(text)) => text,
        Some(Value::Object(map)) => map.get("value").and_then(Value::as_str).unwrap_or_default(),
        _ => "",
    }
}

/// Walks a whole table in pages.
pub struct Walk<'a> {
    client: &'a Client,
    table: String,
    params: TableQuery,
    page: u64,
    /// `None` pages by offset.
    after: Option<String>,
    offset: u64,
    done: bool,
}

impl<'a> Walk<'a> {
    /// Pages by `sys_id`, which never skips or repeats a row while the table
    /// changes, unless the query sorts by itself or `by_offset` is set.
    pub fn new(
        client: &'a Client,
        table: &str,
        mut params: TableQuery,
        page: u64,
        by_offset: bool,
        resume: Option<String>,
    ) -> Self {
        let keyset = !by_offset && !query::has_order(&params.query);
        if keyset && !params.fields.is_empty() && !params.fields.iter().any(|f| f == "sys_id") {
            params.fields.push("sys_id".into());
        }
        params.no_count = true;
        Self {
            client,
            table: table.to_owned(),
            params,
            page: page.max(1),
            after: keyset.then(|| resume.unwrap_or_default()),
            offset: 0,
            done: false,
        }
    }

    /// The last key read, to resume an interrupted walk.
    pub fn resume_key(&self) -> Option<&str> {
        self.after.as_deref().filter(|k| !k.is_empty())
    }

    pub async fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if self.done {
            return Ok(None);
        }
        let mut params = self.params.clone();
        params.limit = Some(self.page);
        match &self.after {
            Some(key) => {
                let bound = if key.is_empty() {
                    String::new()
                } else {
                    format!("sys_id>{key}")
                };
                params.query = query::all([
                    query::bounded(&params.query, &bound).as_str(),
                    "ORDERBYsys_id",
                ]);
            }
            None => params.offset = Some(self.offset),
        }
        let rows = self.client.list(&self.table, &params).await?.rows;
        // ACLs can hide rows after the limit applied, so only an empty page
        // ends a keyset walk.
        self.done = rows.is_empty() || (self.after.is_none() && (rows.len() as u64) < self.page);
        self.offset += rows.len() as u64;
        if let (Some(key), Some(last)) = (&mut self.after, rows.last()) {
            let next = raw(last, "sys_id").to_owned();
            if next.is_empty() || next == *key {
                return Err(Error::invalid(format!(
                    "table `{}` has no usable sys_id to page by; add --paginate offset",
                    self.table
                )));
            }
            *key = next;
        }
        Ok((!rows.is_empty()).then_some(rows))
    }
}
