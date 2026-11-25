//! Arguments that several commands share.

use crate::ctx::{Ctx, read_input};
use clap::{Args, ValueEnum};
use serde_json::{Map, Value};
use sn_core::refs::{self, Record};
use sn_core::{Client, DisplayValue, Error, Result, TableQuery};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Display {
    /// Labels.
    True,
    /// Raw values and sys_ids.
    False,
    /// Both, as {display_value, value}.
    All,
}

impl From<Display> for DisplayValue {
    fn from(value: Display) -> Self {
        match value {
            Display::True => Self::True,
            Display::False => Self::False,
            Display::All => Self::All,
        }
    }
}

#[derive(Args, Debug, Clone, Default)]
pub struct DisplayArg {
    /// Labels (true, the default), raw values (false), or both (all).
    #[arg(long, value_enum, value_name = "MODE", alias = "sysparm-display-value")]
    pub display_value: Option<Display>,
}

impl DisplayArg {
    pub fn value(&self) -> DisplayValue {
        self.display_value.map(Into::into).unwrap_or_default()
    }
}

/// `--query`, `--fields`, `--limit`, `--offset` and the display flags.
#[derive(Args, Debug, Clone, Default)]
pub struct ListArgs {
    /// Encoded query, for example active=true^priority=1.
    #[arg(long, short = 'q', value_name = "QUERY", alias = "sysparm-query")]
    pub query: Option<String>,
    /// Fields to read, comma-separated; dots follow references.
    #[arg(
        long,
        short = 'f',
        value_delimiter = ',',
        value_name = "FIELDS",
        alias = "sysparm-fields"
    )]
    pub fields: Vec<String>,
    /// Most records to return.
    #[arg(long, short = 'n', value_name = "N", aliases = ["setlimit", "setLimit", "page-size", "sysparm-limit"])]
    pub limit: Option<u64>,
    /// Records to skip.
    #[arg(long, value_name = "N", alias = "sysparm-offset")]
    pub offset: Option<u64>,
    #[command(flatten)]
    pub display: DisplayArg,
    #[command(flatten)]
    pub extra: ReadArgs,
}

/// Less common `sysparm_*` parameters.
#[derive(Args, Debug, Clone, Default)]
pub struct ReadArgs {
    /// Keep the links of reference fields.
    #[arg(long)]
    pub reference_links: bool,
    /// Leave the links of reference fields out (the default).
    #[arg(long, hide = true, alias = "sysparm-exclude-reference-link")]
    pub exclude_reference_link: bool,
    /// Use the fields of this form or list view.
    #[arg(long, value_name = "VIEW", alias = "sysparm-view")]
    pub view: Option<String>,
    /// Index hint for the query.
    #[arg(long, value_name = "NAME", alias = "sysparm-query-category")]
    pub query_category: Option<String>,
    /// Search across domains.
    #[arg(long, alias = "sysparm-query-no-domain")]
    pub query_no_domain: bool,
    /// Skip counting the matches (faster).
    #[arg(long, alias = "sysparm-no-count")]
    pub no_count: bool,
    /// Accepted for compatibility: pagination headers are never shown.
    #[arg(long, hide = true, alias = "sysparm-suppress-pagination-header")]
    pub suppress_pagination_header: bool,
}

impl ReadArgs {
    pub fn fill(&self, query: &mut TableQuery) {
        query.reference_links = self.reference_links;
        query.view = self.view.clone();
        query.query_category = self.query_category.clone();
        query.query_no_domain = self.query_no_domain;
        query.no_count = self.no_count;
    }
}

impl ListArgs {
    pub fn to_query(&self, default_limit: u64) -> TableQuery {
        let mut query = TableQuery {
            query: self.query.clone().unwrap_or_default(),
            fields: self.fields.clone(),
            limit: Some(self.limit.unwrap_or(default_limit)),
            offset: self.offset,
            display: self.display.value(),
            ..TableQuery::default()
        };
        self.extra.fill(&mut query);
        query
    }

    /// The `meta` of a page: the total, and how to get the next page.
    pub fn meta(&self, returned: usize, total: Option<u64>, default_limit: u64) -> Value {
        let mut meta = Map::new();
        let offset = self.offset.unwrap_or(0);
        let next = offset + returned as u64;
        if let Some(total) = total {
            meta.insert("total".into(), total.into());
        }
        let full = returned as u64 >= self.limit.unwrap_or(default_limit);
        if full && total.is_none_or(|total| next < total) && returned > 0 {
            meta.insert("next_offset".into(), next.into());
            meta.insert("next".into(), format!("add --offset {next}").into());
        }
        meta.into()
    }
}

/// `--data` and `--field`: the body of a write.
#[derive(Args, Debug, Clone, Default)]
pub struct BodyArgs {
    /// JSON object, @file, or - for stdin.
    #[arg(long, short = 'D', value_name = "JSON")]
    pub data: Option<String>,
    /// name=value (repeatable); name=@file reads the value from a file.
    #[arg(
        long = "field",
        short = 'F',
        value_name = "NAME=VALUE",
        allow_hyphen_values = true
    )]
    pub field: Vec<String>,
}

impl BodyArgs {
    /// The body, `--field` over `--data`.
    pub fn object(&self) -> Result<Map<String, Value>> {
        let mut body = match &self.data {
            None => Map::new(),
            Some(text) => match parse_json(&read_input(text)?, "--data")? {
                Value::Object(map) => map,
                _ => {
                    return Err(Error::invalid(
                        "--data is JSON but not an object; pass an object such as '{\"short_description\":\"Disk full\"}'",
                    ));
                }
            },
        };
        for (name, value) in pairs(&self.field, "--field")? {
            body.insert(name, read_field(&value)?.into());
        }
        Ok(body)
    }

    /// The body, or an error when it has no field.
    pub fn required(&self) -> Result<Map<String, Value>> {
        let body = self.object()?;
        match body.is_empty() {
            true => Err(Error::Input {
                arg: "field".into(),
                message: "there is nothing to write; pass --field name=value (repeatable) or --data '{...}'".into(),
                choices: Vec::new(),
            }),
            false => Ok(body),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_none() && self.field.is_empty()
    }
}

/// A field value: text, or `@file`.
fn read_field(value: &str) -> Result<String> {
    match value.strip_prefix('@') {
        Some(path) if !path.is_empty() && path != "-" && !path.starts_with(' ') => {
            // `@me` and mail handles are values, not files.
            match std::path::Path::new(path).is_file() {
                true => read_input(value),
                false => Ok(value.to_owned()),
            }
        }
        Some("-") => read_input(value),
        _ => Ok(value.to_owned()),
    }
}

pub fn parse_json(text: &str, what: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|err| {
        Error::invalid(format!(
            "{what} is not valid JSON ({err}); pass JSON, @file.json, or - to read stdin"
        ))
    })
}

/// Splits `name=value` arguments at the first `=`.
pub fn pairs(items: &[String], flag: &str) -> Result<Vec<(String, String)>> {
    items
        .iter()
        .map(|item| match item.split_once('=') {
            Some((name, value)) if !name.trim().is_empty() => {
                Ok((name.trim().to_owned(), value.to_owned()))
            }
            _ => Err(Error::invalid(format!(
                "you passed {flag} {item}, which has no name before `=`; use {flag} name=value"
            ))),
        })
        .collect()
}

/// The name and value of a `--header 'Name: value'`.
pub fn header(text: &str) -> Result<(&str, &str)> {
    let (name, value) = text.split_once(':').ok_or_else(|| {
        Error::invalid(format!(
            "you passed --header {text}, which has no colon; use --header 'Name: value'"
        ))
    })?;
    match name.trim() {
        name if name.eq_ignore_ascii_case("authorization") => Err(Error::invalid(
            "the Authorization header comes from the profile; choose another --profile to change who you are",
        )),
        name => Ok((name, value.trim())),
    }
}

/// `TABLE SYS_ID`, `table:number`, a number, or a link.
#[derive(Args, Debug, Clone)]
pub struct RecordArgs {
    /// Table, or the record itself: a number, table:number, table:sys_id, or a link.
    #[arg(value_name = "TABLE|RECORD")]
    pub table: String,
    /// sys_id or number when the first argument is a table.
    #[arg(value_name = "SYS_ID|NUMBER")]
    pub id: Option<String>,
}

impl RecordArgs {
    pub async fn record(&self, client: &Client) -> Result<Record> {
        refs::record(client, &self.table, self.id.as_deref()).await
    }
}

/// Finds one record of a known table from a number, sys_id or link.
pub async fn record_of(client: &Client, table: &str, id: &str) -> Result<String> {
    Ok(refs::record(client, table, Some(id)).await?.sys_id)
}

/// The text of `--text`-like inputs: the argument, or `--file`.
pub fn text_or_file(text: Option<&str>, file: Option<&str>, what: &str) -> Result<String> {
    let content = match (text, file) {
        (Some(text), None) => text.to_owned(),
        (None, Some("-")) => read_input("-")?,
        (None, Some(path)) => read_input(&format!("@{path}"))?,
        (Some(_), Some(_)) => {
            return Err(Error::invalid(format!(
                "you passed {what} twice; pass the text or a file, not both"
            )));
        }
        (None, None) => {
            return Err(Error::Input {
                arg: what.into(),
                message: format!("{what} is missing; pass the text, or a file (- for stdin)"),
                choices: Vec::new(),
            });
        }
    };
    match content.trim().is_empty() {
        true => Err(Error::invalid(format!("{what} is empty; pass some text"))),
        false => Ok(content),
    }
}

/// Prints records with the `--raw` envelope when it is on.
pub fn show(ctx: &Ctx, value: Value) -> Result<()> {
    match ctx.out.raw {
        true => ctx.out.item(serde_json::json!({ "result": value })),
        false => ctx.out.any(value),
    }
}

/// `--limit` and `--offset` of lists that are not Table API lists.
#[derive(Args, Debug, Clone, Default)]
pub struct PageArgs {
    /// Most results to return.
    #[arg(long, short = 'n', value_name = "N", aliases = ["setlimit", "setLimit", "page-size", "sysparm-limit"])]
    pub limit: Option<u64>,
    /// Results to skip.
    #[arg(long, value_name = "N", alias = "sysparm-offset")]
    pub offset: Option<u64>,
}

impl PageArgs {
    pub fn apply(&self, request: sn_core::Request, default_limit: u64) -> sn_core::Request {
        request
            .query(
                "sysparm_limit",
                self.limit.unwrap_or(default_limit).to_string(),
            )
            .query_opt("sysparm_offset", self.offset.map(|o| o.to_string()))
    }

    /// The `meta` of a page: the total, and how to get the next page.
    pub fn meta(&self, returned: usize, total: Option<u64>, default_limit: u64) -> Value {
        let list = ListArgs {
            limit: self.limit,
            offset: self.offset,
            ..ListArgs::default()
        };
        list.meta(returned, total, default_limit)
    }
}

/// The rows that came back are not the rows that were asked for: the
/// instance drops query terms it does not know and answers with any records.
pub fn dropped(term: &str, table: &str) -> Error {
    Error::api(
        format!(
            "the instance ignored the filter on {term} of `{table}`, so its answer is about other records; check that the table has this field and that you can read it"
        ),
        Value::Null,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_merge_fields_over_data() {
        let body = BodyArgs {
            data: Some(r#"{"a":"1","b":2}"#.into()),
            field: vec!["a=x=y".into(), "c=@me".into()],
        };
        let map = body.object().expect("body");
        assert_eq!(
            Value::from(map),
            serde_json::json!({ "a": "x=y", "b": 2, "c": "@me" })
        );
        assert!(
            BodyArgs {
                data: Some("[1]".into()),
                field: vec![]
            }
            .object()
            .is_err()
        );
        assert!(
            BodyArgs {
                data: None,
                field: vec!["=x".into()]
            }
            .object()
            .is_err()
        );
        assert!(BodyArgs::default().required().is_err());
    }
}
