//! Encoded queries: ServiceNow's filter syntax.

use crate::error::{Error, Result};

/// One term, `field<operator>value`. A `^` inside the value would start a
/// new term, so it is doubled, which the platform reads as a literal.
pub fn term(field: &str, operator: &str, value: &str) -> String {
    format!("{field}{operator}{}", value.replace('^', "^^"))
}

/// The terms between the `^` separators; a doubled `^` is part of a value.
/// The first term of another alternative keeps its `NQ`, an OR term its `OR`.
pub fn terms(query: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(query);
    std::iter::from_fn(move || {
        let text = rest?;
        let mut from = 0;
        while let Some(at) = text[from..].find('^').map(|at| from + at) {
            if text[at + 1..].starts_with('^') {
                from = at + 2;
                continue;
            }
            rest = Some(&text[at + 1..]);
            return Some(&text[..at]);
        }
        rest = None;
        Some(text)
    })
}

/// The terms of each `^NQ` alternative.
pub fn alternatives(query: &str) -> Vec<Vec<&str>> {
    let mut out = vec![Vec::new()];
    for term in terms(query) {
        match (term.strip_prefix("NQ"), out.last_mut()) {
            (None, Some(last)) => last.push(term),
            (first, _) => out.push(vec![first.unwrap_or(term)]),
        }
    }
    out
}

/// Joins the non-empty parts with `^` (AND).
pub fn all<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for term in parts
        .into_iter()
        .flat_map(|part| terms(part.trim_start_matches('^')))
        .filter(|term| !term.is_empty())
    {
        if !out.is_empty() {
            out.push('^');
        }
        out.push_str(term);
    }
    out
}

/// Refuses names that would change the path or the query they go into.
pub fn check_name(what: &str, name: &str) -> Result<()> {
    if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "you passed the {what} `{name}`; a {what} has only letters, digits and underscores, for example incident or u_custom"
    )))
}

/// Keyset paging over `sys_id` needs every alternative of a `^NQ` query
/// bounded, so the bound goes in front of each alternative's terms.
pub fn bounded(query: &str, bound: &str) -> String {
    alternatives(query)
        .into_iter()
        .map(|terms| all(terms.into_iter().chain([bound])))
        .collect::<Vec<_>>()
        .join("^NQ")
}

/// True when the query sorts by itself.
pub fn has_order(query: &str) -> bool {
    terms(query).any(|term| term.starts_with("ORDERBY") || term.starts_with("GROUPBY"))
}

/// The query, sorted by `order` unless it sorts by itself.
pub fn ordered(query: &str, order: &str) -> String {
    match has_order(query) {
        true => query.to_owned(),
        false => all([query, order]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_escape_carets_and_join() {
        assert_eq!(term("name", "=", "a^b"), "name=a^^b");
        assert_eq!(
            all(["active=true", "", "^priority=1^"]),
            "active=true^priority=1"
        );
        assert_eq!(
            bounded("a=1^NQb=2", "sys_id>x"),
            "a=1^sys_id>x^NQb=2^sys_id>x"
        );
        assert!(has_order("a=1^ORDERBYDESCname"));
        assert!(check_name("table", "incident").is_ok());
        assert!(check_name("table", "a/b").is_err());
    }

    #[test]
    fn a_doubled_caret_belongs_to_the_value() {
        let name = term("name", "=", "a^");
        assert_eq!(all([name.as_str(), "x=1"]), "name=a^^^x=1");
        assert_eq!(
            terms("name=a^^^x=1^NQy=2").collect::<Vec<_>>(),
            ["name=a^^", "x=1", "NQy=2"]
        );
        assert_eq!(bounded("name=a^^NQb", "sys_id>x"), "name=a^^NQb^sys_id>x");
        assert_eq!(bounded("", "sys_id>x"), "sys_id>x");
        assert!(!has_order("name=a^^ORDERBYx"));
    }

    #[test]
    fn an_own_order_is_kept() {
        assert_eq!(ordered("", "ORDERBYname"), "ORDERBYname");
        assert_eq!(ordered("a=1", "ORDERBYname"), "a=1^ORDERBYname");
        assert_eq!(ordered("a=1^ORDERBYb", "ORDERBYname"), "a=1^ORDERBYb");
    }
}
