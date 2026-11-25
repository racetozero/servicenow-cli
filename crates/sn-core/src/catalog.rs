//! Every operation of ServiceNow's public REST reference, offline.
//! `scripts/build_catalog.py` writes the data.

use crate::error::{Choice, Error, Result};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::sync::OnceLock;

const DATA: &str = include_str!("../catalog/catalog.json");

/// One page of the reference. Its operations are parsed on demand: a command
/// needs one API of about 140.
#[derive(Deserialize)]
pub struct Api {
    pub name: &'static str,
    pub title: String,
    pub text: String,
    pub doc: &'static str,
    #[serde(borrow)]
    operations: &'static RawValue,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Operation {
    pub id: String,
    pub method: String,
    pub path: String,
    pub summary: String,
    pub params: Vec<Param>,
    /// Content type of the body, when it is not JSON.
    pub content: Option<String>,
    /// A GET that changes something.
    #[serde(default)]
    pub writes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Place {
    Path,
    Query,
    Body,
}

impl Place {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Query => "query",
            Self::Body => "body",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Param {
    pub name: String,
    pub place: Place,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub fields: Vec<Param>,
}

impl Param {
    /// The command-line name: `sysparm_query` is `--query`.
    pub fn flag(&self) -> String {
        let name = self.name.trim_prefix("sysparm_");
        let mut out = String::with_capacity(name.len() + 4);
        let mut lower = false;
        for c in name.chars() {
            if c.is_ascii_uppercase() {
                if lower {
                    out.push('-');
                }
                out.push(c.to_ascii_lowercase());
            } else if c.is_ascii_alphanumeric() {
                out.push(c);
            } else if !out.ends_with('-') {
                out.push('-');
            }
            lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        }
        out.trim_matches('-').to_owned()
    }
}

impl Api {
    pub fn operations(&self) -> Vec<Operation> {
        serde_json::from_str(self.operations.get()).unwrap_or_default()
    }

    pub fn operation(&self, id: &str) -> Result<Operation> {
        let operations = self.operations();
        match operations.iter().find(|o| o.id == id) {
            Some(operation) => Ok(operation.clone()),
            None => Err(Error::Input {
                arg: "operation".into(),
                message: format!(
                    "the {} API has no operation `{id}`; pass one of its operations (sn api {})",
                    self.name, self.name
                ),
                choices: operations
                    .iter()
                    .map(|o| Choice::new(&o.id, format!("{} {}", o.method, o.path)))
                    .collect(),
            }),
        }
    }
}

pub fn apis() -> &'static [Api] {
    static APIS: OnceLock<Vec<Api>> = OnceLock::new();
    APIS.get_or_init(|| serde_json::from_str(DATA).unwrap_or_default())
}

pub fn api(name: &str) -> Result<&'static Api> {
    apis().iter().find(|a| a.name == name).ok_or_else(|| Error::Input {
        arg: "api".into(),
        message: format!("there is no API `{name}`; find it with `sn api search TEXT`, or list all with `sn api list`"),
        choices: apis()
            .iter()
            .filter(|a| a.name.contains(name) || a.title.to_ascii_lowercase().contains(&name.to_ascii_lowercase()))
            .map(|a| Choice::new(a.name, &a.title))
            .collect(),
    })
}

/// Operations that contain every word, best matches first.
pub fn search(text: &str, method: Option<&str>) -> Vec<(&'static Api, Operation)> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    let mut found = Vec::new();
    for api in apis() {
        // Cheap test first: parsing all operations costs more.
        let page = api.operations.get().to_ascii_lowercase();
        let title = api.title.to_ascii_lowercase();
        if !words
            .iter()
            .all(|w| page.contains(w) || api.name.contains(w.as_str()) || title.contains(w))
        {
            continue;
        }
        for operation in api.operations() {
            if method.is_some_and(|m| !m.eq_ignore_ascii_case(&operation.method)) {
                continue;
            }
            let strong =
                format!("{} {} {}", api.name, operation.id, operation.path).to_ascii_lowercase();
            let all = format!(
                "{strong} {title} {}",
                operation.summary.to_ascii_lowercase()
            );
            if words.iter().all(|w| all.contains(w.as_str())) {
                let score = words.iter().filter(|w| strong.contains(w.as_str())).count();
                found.push((score, api, operation));
            }
        }
    }
    found.sort_by_key(|(score, _, _)| std::cmp::Reverse(*score));
    found
        .into_iter()
        .map(|(_, api, operation)| (api, operation))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn the_catalog_is_complete_and_unambiguous() {
        let mut total = 0;
        for api in apis() {
            let operations = api.operations();
            assert!(!operations.is_empty(), "{} has no operations", api.name);
            let ids: HashSet<&str> = operations.iter().map(|o| o.id.as_str()).collect();
            assert_eq!(ids.len(), operations.len(), "{} repeats an id", api.name);
            for operation in &operations {
                assert!(operation.path.starts_with("/api/"), "{}", operation.path);
                for param in operation.params.iter().filter(|p| p.place == Place::Path) {
                    assert!(
                        operation.path.contains(&format!("{{{}}}", param.name)),
                        "{} {}",
                        operation.path,
                        param.name
                    );
                }
            }
            // Two operations with one path are a mistake of the reference;
            // only this API tells its messages apart by the body.
            let mut routes = HashSet::new();
            for o in operations
                .iter()
                .filter(|_| api.name != "custom-connector-gateway")
            {
                assert!(
                    routes.insert((&o.method, &o.path)),
                    "{} {} {}",
                    api.name,
                    o.method,
                    o.path
                );
            }
            total += operations.len();
        }
        assert!(
            apis().len() >= 141 && total >= 712,
            "{} APIs, {total} operations",
            apis().len()
        );
    }

    #[test]
    fn flags_are_readable() {
        let flag = |name: &str| {
            Param {
                name: name.into(),
                place: Place::Query,
                kind: None,
                required: false,
                text: String::new(),
                fields: vec![],
            }
            .flag()
        };
        assert_eq!(flag("sysparm_display_value"), "display-value");
        assert_eq!(flag("tableName"), "table-name");
        assert_eq!(flag("sys_id"), "sys-id");
        assert_eq!(flag("@type"), "type");
    }

    #[test]
    fn search_finds_by_words() {
        let found = search("change conflict", Some("get"));
        assert!(
            found
                .iter()
                .any(|(a, o)| a.name == "change-management" && o.id == "list-conflict")
        );
    }
}
