//! Encoded queries as conditions a person can edit, and back.

use crate::model::{Field, Kind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Is,
    IsNot,
    Contains,
    Lacks,
    Starts,
    Ends,
    OneOf,
    Empty,
    Filled,
    More,
    MoreOrEqual,
    Less,
    LessOrEqual,
    LastDays,
    OlderThanDays,
}

impl Op {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Is => "is",
            Self::IsNot => "is not",
            Self::Contains => "contains",
            Self::Lacks => "does not contain",
            Self::Starts => "starts with",
            Self::Ends => "ends with",
            Self::OneOf => "is one of",
            Self::Empty => "is empty",
            Self::Filled => "is not empty",
            Self::More => "is more than",
            Self::MoreOrEqual => "is at least",
            Self::Less => "is less than",
            Self::LessOrEqual => "is at most",
            Self::LastDays => "in the last days",
            Self::OlderThanDays => "older than days",
        }
    }

    const fn code(self) -> &'static str {
        match self {
            Self::Is => "=",
            Self::IsNot => "!=",
            Self::Contains => "LIKE",
            Self::Lacks => "NOT LIKE",
            Self::Starts => "STARTSWITH",
            Self::Ends => "ENDSWITH",
            Self::OneOf => "IN",
            Self::Empty => "ISEMPTY",
            Self::Filled => "ISNOTEMPTY",
            Self::More => ">",
            Self::MoreOrEqual => ">=",
            Self::Less => "<",
            Self::LessOrEqual => "<=",
            Self::LastDays => "RELATIVEGT@dayofweek@ago@",
            Self::OlderThanDays => "RELATIVELT@dayofweek@ago@",
        }
    }

    pub const fn needs_value(self) -> bool {
        !matches!(self, Self::Empty | Self::Filled)
    }

    /// The operators that make sense for a kind of field, best first.
    pub const fn of(kind: Kind) -> &'static [Op] {
        match kind {
            Kind::Bool => &[Self::Is],
            Kind::Choice => &[Self::Is, Self::IsNot, Self::Empty, Self::Filled],
            Kind::Reference => &[Self::Is, Self::IsNot, Self::Empty, Self::Filled],
            Kind::Date | Kind::DateTime => &[
                Self::LastDays,
                Self::OlderThanDays,
                Self::MoreOrEqual,
                Self::LessOrEqual,
                Self::Empty,
                Self::Filled,
            ],
            Kind::Number => &[
                Self::Is,
                Self::IsNot,
                Self::MoreOrEqual,
                Self::LessOrEqual,
                Self::Empty,
                Self::Filled,
            ],
            _ => &[
                Self::Contains,
                Self::Is,
                Self::IsNot,
                Self::Starts,
                Self::Ends,
                Self::Lacks,
                Self::Empty,
                Self::Filled,
            ],
        }
    }
}

/// Longest code first, so `>=` is not read as `>` and a value.
const PARSE: &[Op] = &[
    Op::LastDays,
    Op::OlderThanDays,
    Op::Filled,
    Op::Empty,
    Op::Starts,
    Op::Ends,
    Op::Lacks,
    Op::Contains,
    Op::OneOf,
    Op::IsNot,
    Op::MoreOrEqual,
    Op::LessOrEqual,
    Op::Is,
    Op::More,
    Op::Less,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Join {
    #[default]
    And,
    /// Or with the condition above.
    Or,
    /// A new alternative: everything above, or this and what follows.
    New,
}

impl Join {
    const fn code(self) -> &'static str {
        match self {
            Self::And => "^",
            Self::Or => "^OR",
            Self::New => "^NQ",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Condition {
    pub join: Join,
    pub field: String,
    pub op: Op,
    pub value: String,
    /// What a person reads for the value.
    pub label: String,
    /// A term this editor does not know; it is kept as it is.
    pub raw: Option<String>,
}

impl Condition {
    pub fn new(field: &Field) -> Self {
        Self {
            join: Join::And,
            field: field.name.clone(),
            op: Op::of(field.kind)[0],
            value: match field.kind {
                Kind::Bool => "true".into(),
                _ => String::new(),
            },
            label: match field.kind {
                Kind::Bool => "Yes".into(),
                _ => String::new(),
            },
            raw: None,
        }
    }

    pub fn complete(&self) -> bool {
        self.raw.is_some() || !self.op.needs_value() || !self.value.is_empty()
    }

    fn term(&self) -> String {
        match &self.raw {
            Some(raw) => raw.clone(),
            None if self.op.needs_value() => {
                sn_core::query::term(&self.field, self.op.code(), &self.value)
            }
            None => format!("{}{}", self.field, self.op.code()),
        }
    }
}

/// `field` and whether it sorts downwards.
pub type Order = Vec<(String, bool)>;

pub fn order_text(order: &[(String, bool)]) -> String {
    order
        .iter()
        .map(|(f, down)| format!("{}{f}", if *down { "ORDERBYDESC" } else { "ORDERBY" }))
        .collect::<Vec<_>>()
        .join("^")
}

/// Splits a query at `^`, where `^^` is a literal caret of a value.
fn split_terms(query: &str) -> Vec<&str> {
    sn_core::query::terms(query)
        .filter(|t| !t.trim().is_empty() && *t != "EQ")
        .collect()
}

/// The query without its order, and the order.
pub fn split_order(query: &str) -> (String, Order) {
    let mut order = Order::new();
    let mut rest = Vec::new();
    for term in split_terms(query) {
        if let Some(field) = term.strip_prefix("ORDERBYDESC") {
            order.push((field.to_owned(), true));
        } else if let Some(field) = term.strip_prefix("ORDERBY") {
            order.push((field.to_owned(), false));
        } else {
            rest.push(term);
        }
    }
    (rest.join("^"), order)
}

pub fn parse(query: &str) -> Vec<Condition> {
    split_terms(query)
        .into_iter()
        .map(|term| {
            let (join, body) = if let Some(rest) = term.strip_prefix("NQ") {
                (Join::New, rest)
            } else if let Some(rest) = term
                .strip_prefix("OR")
                .filter(|_| !term.starts_with("ORDER"))
            {
                (Join::Or, rest)
            } else {
                (Join::And, term)
            };
            let name_length = body
                .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || "_.".contains(c)))
                .unwrap_or(body.len());
            let (field, tail) = body.split_at(name_length);
            let found = PARSE.iter().find_map(|op| {
                let value = tail.strip_prefix(op.code())?;
                (op.needs_value() || value.is_empty()).then_some((*op, value))
            });
            match found.filter(|_| !field.is_empty()) {
                Some((op, value)) => Condition {
                    join,
                    field: field.to_owned(),
                    op,
                    value: value.replace("^^", "^"),
                    label: value.replace("^^", "^"),
                    raw: None,
                },
                None => Condition {
                    join,
                    field: String::new(),
                    op: Op::Is,
                    value: String::new(),
                    label: String::new(),
                    raw: Some(body.to_owned()),
                },
            }
        })
        .collect()
}

pub fn build(conditions: &[Condition]) -> String {
    let mut out = String::new();
    for c in conditions.iter().filter(|c| c.complete()) {
        if !out.is_empty() {
            out.push_str(c.join.code());
        }
        out.push_str(&c.term());
    }
    out
}

/// A query as a sentence. `label` names a field, `value` a value of it.
pub fn describe(
    query: &str,
    label: impl Fn(&str) -> String,
    value: impl Fn(&str, &str) -> String,
) -> String {
    let mut out = String::new();
    for c in parse(query) {
        if !out.is_empty() {
            out.push_str(match c.join {
                Join::And => " and ",
                Join::Or => " or ",
                Join::New => ", or: ",
            });
        }
        out.push_str(&match (&c.raw, c.op) {
            (Some(raw), _) => raw.clone(),
            (None, Op::LastDays) => format!("{} in the last {} days", label(&c.field), c.value),
            (None, Op::OlderThanDays) => {
                format!("{} older than {} days", label(&c.field), c.value)
            }
            (None, Op::Is) => format!("{} = {}", label(&c.field), value(&c.field, &c.value)),
            (None, op) if op.needs_value() => format!(
                "{} {} {}",
                label(&c.field),
                op.label(),
                value(&c.field, &c.value)
            ),
            (None, op) => format!("{} {}", label(&c.field), op.label()),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_survive_the_round_trip() {
        for query in [
            "active=true^assigned_to=javascript:gs.getUserID()",
            "priority<=2^ORpriority=3^short_descriptionLIKEvpn^^x",
            "active=true^NQstate=6",
            "sys_created_onRELATIVEGT@dayofweek@ago@7",
            "assigned_toISEMPTY^nameNOT LIKEa b",
            "stateIN1,2,3",
            "123TEXTQUERY321=mail server",
            "assignment_groupDYNAMICd6435e965f510100a9ad2572f2b47744",
            "caller_id.vip=true",
        ] {
            assert_eq!(build(&parse(query)), query, "{query}");
        }
    }

    #[test]
    fn conditions_read_operators_exactly() {
        let one = |q: &str| {
            let c = parse(q).remove(0);
            (c.field, c.op, c.value)
        };
        assert_eq!(
            one("priority>=2"),
            ("priority".into(), Op::MoreOrEqual, "2".into())
        );
        assert_eq!(one("a!=b"), ("a".into(), Op::IsNot, "b".into()));
        assert_eq!(
            one("closed_atISNOTEMPTY"),
            ("closed_at".into(), Op::Filled, String::new())
        );
        let parsed = parse("123TEXTQUERY321=x");
        assert!(parsed[0].raw.is_some());
    }

    #[test]
    fn the_order_is_separate() {
        let (query, order) =
            split_order("active=true^ORDERBYpriority^ORstate=1^ORDERBYDESCsys_updated_on");
        assert_eq!(query, "active=true^ORstate=1");
        assert_eq!(
            order,
            [
                ("priority".to_owned(), false),
                ("sys_updated_on".to_owned(), true)
            ]
        );
        assert_eq!(
            order_text(&order),
            "ORDERBYpriority^ORDERBYDESCsys_updated_on"
        );
    }

    #[test]
    fn an_unfinished_condition_is_left_out() {
        let mut list = parse("active=true^state=1");
        list[1].value.clear();
        assert_eq!(build(&list), "active=true");
    }
}
