//! Catalog items, their variables, and the checks of an order that need no
//! network.
//!
//! The order endpoint enforces only static mandatory flags. It stores or
//! drops invalid choices, references to the wrong table, unknown names and
//! malformed dates without a word; the portal is what normally prevents
//! that. This module does it for orders from the command line.

use serde_json::{Map, Value, json};
use sn_core::{Error, Result, is_sys_id};
use std::collections::{BTreeMap, BTreeSet};

/// Shown instead of a sensitive value.
pub const REDACTED: &str = "[REDACTED]";
pub const DATE_FORMAT: &str = "yyyy-MM-dd";
pub const TIME_FORMAT: &str = "HH:mm:ss";
/// The instance's own flag, which not every release sets reliably.
pub const UNORDERABLE: &str = "the instance reports this item as not orderable through the API; ordering may only leave it in a cart";

/// Classes that a search returns but an order cannot have.
const NOT_ORDERABLE: &[(&str, &str)] = &[
    ("sc_cat_item_producer", "record producer"),
    ("sc_cat_item_composite_producer", "record producer"),
    ("std_change_record_producer", "standard change template"),
    ("sc_cat_item_guide", "order guide"),
    ("sc_cat_item_content", "content item"),
    ("sc_cat_item_wizard", "catalog wizard"),
];
/// Form elements without a value. The type codes stay reliable when a
/// friendly type is misspelt or new.
const LAYOUT: &[&str] = &[
    "container_end",
    "container_split",
    "label",
    "rich_text_label",
    "break",
];
const LAYOUT_CODES: &[i64] = &[11, 12, 20, 24, 32];
/// Elements that the portal draws with its own code and that may collect
/// input.
const WIDGETS: &[&str] = &[
    "macro",
    "macro_with_label",
    // Some instances spell it like this.
    "macro_with_abel",
    "ui_page",
    "custom",
    "custom_with_label",
];
const WIDGET_CODES: &[i64] = &[14, 15, 17];

/// A short name for an item class.
pub fn kind_label(class: &str) -> &str {
    match class {
        "sc_cat_item" => "item",
        "pc_hardware_cat_item" => "hardware",
        "pc_software_cat_item" => "software",
        other => NOT_ORDERABLE
            .iter()
            .find(|(name, _)| *name == other)
            .map_or(other, |(_, label)| label),
    }
}

pub fn is_orderable_class(class: &str) -> bool {
    !NOT_ORDERABLE.iter().any(|(name, _)| *name == class)
}

/// Names of fields and variables that carry secrets.
pub fn is_sensitive_name(name: &str) -> bool {
    let name: String = name
        .chars()
        .map(|c| match c.is_ascii_alphanumeric() {
            true => c.to_ascii_lowercase(),
            false => '_',
        })
        .collect();
    let parts = [
        "password",
        "passwd",
        "secret",
        "private_key",
        "api_key",
        "apikey",
        "access_key",
        "passphrase",
        "passcode",
    ];
    let words = [
        "token",
        "cookie",
        "credential",
        "credentials",
        "authorization",
        "pwd",
        "pw",
        "pass",
        "pin",
        "ssn",
        "otp",
    ];
    parts.iter().any(|part| name.contains(part)) || name.split('_').any(|w| words.contains(&w))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    MultiLineText,
    Masked,
    CheckBox,
    YesNo,
    Choice,
    Reference,
    ListCollector,
    Date,
    DateTime,
    Attachment,
    MultiRowSet,
    Unknown,
}

impl Kind {
    fn of(friendly: &str) -> Self {
        match friendly {
            "single_line_text" | "wide_single_line_text" | "email" | "url" | "ip_address" => {
                Self::Text
            }
            "multi_line_text" | "html" => Self::MultiLineText,
            "masked" => Self::Masked,
            "check_box" => Self::CheckBox,
            "yes_no" => Self::YesNo,
            "select_box" | "multiple_choice" | "lookup_select_box" | "lookup_multiple_choice" => {
                Self::Choice
            }
            "reference" | "requested_for" => Self::Reference,
            "list_collector" => Self::ListCollector,
            "date" => Self::Date,
            "date_time" => Self::DateTime,
            "attachment" => Self::Attachment,
            "sc_multi_row" => Self::MultiRowSet,
            _ => Self::Unknown,
        }
    }

    /// An unknown kind is assumed to carry a value, so nothing is dropped.
    fn has_value(self) -> bool {
        !matches!(self, Self::Attachment | Self::MultiRowSet)
    }
}

#[derive(Debug, Clone)]
pub struct Variable {
    pub name: String,
    pub label: String,
    /// The instance's `friendly_type`, for display.
    pub type_name: String,
    pub kind: Kind,
    pub mandatory: bool,
    pub read_only: bool,
    pub default: Option<String>,
    /// Value and label.
    pub choices: Vec<(String, String)>,
    /// The table of references.
    pub table: Option<String>,
    pub qualifier: Option<String>,
    /// Label of the container around it.
    pub container: Option<String>,
}

impl Variable {
    /// Masked variables, and free text whose name or label looks like a
    /// secret. A checkbox `requires_password` cannot hold one.
    pub fn is_sensitive(&self) -> bool {
        match self.kind {
            Kind::Masked => true,
            Kind::Text | Kind::MultiLineText | Kind::Unknown => {
                is_sensitive_name(&self.name) || is_sensitive_name(&self.label)
            }
            _ => false,
        }
    }

    /// A sensitive default is hidden.
    pub fn shown_default(&self) -> Option<&str> {
        match self.default.as_deref() {
            Some(value) if !value.is_empty() && self.is_sensitive() => Some(REDACTED),
            other => other,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "label": self.label,
            "type": self.type_name,
            "mandatory": self.mandatory,
            "read_only": self.read_only,
            "default": self.shown_default(),
            "choices": self.choices.iter().map(|(value, label)| json!({ "value": value, "label": label })).collect::<Vec<_>>(),
            "reference_table": self.table,
            "reference_qualifier": self.qualifier,
            "container": self.container,
            "sensitive": self.is_sensitive(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub sys_id: String,
    pub name: String,
    pub short_description: String,
    pub class: String,
    show_quantity: Option<bool>,
    mandatory_attachment: bool,
    /// `Some(false)` warns only: see [`UNORDERABLE`].
    pub instance_orderable: Option<bool>,
    /// Catalog UI policies or client scripts, which only the portal runs.
    pub client_logic: bool,
    pub variables: Vec<Variable>,
    /// Labels of portal widgets.
    pub widgets: Vec<String>,
}

fn string(value: &Value, field: &str) -> Option<String> {
    value[field].as_str().map(str::to_owned)
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(_) | Value::Number(_) => Some(value.to_string()),
        _ => None,
    }
}

impl Item {
    /// From `GET /items/{sys_id}`.
    pub fn parse(value: &Value) -> Result<Self> {
        let sys_id = string(value, "sys_id").ok_or_else(|| {
            Error::api(
                "the instance described a catalog item without a sys_id",
                value.clone(),
            )
        })?;
        let filled = |v: &Value| v.as_array().is_some_and(|a| !a.is_empty());
        let scripts = value["client_script"]
            .as_object()
            .is_some_and(|scripts| scripts.values().any(filled));
        let mut item = Self {
            sys_id,
            name: string(value, "name").unwrap_or_default(),
            short_description: string(value, "short_description").unwrap_or_default(),
            class: string(value, "sys_class_name").unwrap_or_default(),
            show_quantity: value["show_quantity"].as_bool(),
            mandatory_attachment: value["mandatory_attachment"] == true,
            instance_orderable: value["orderable"].as_bool(),
            client_logic: filled(&value["ui_policy"]) || scripts,
            variables: Vec::new(),
            widgets: Vec::new(),
        };
        item.add(&value["variables"], None);
        Ok(item)
    }

    fn add(&mut self, entries: &Value, container: Option<&str>) {
        for entry in entries.as_array().into_iter().flatten() {
            let friendly = string(entry, "friendly_type").unwrap_or_default();
            let label = string(entry, "label").filter(|l| !l.trim().is_empty());
            if friendly == "container_start" {
                self.add(&entry["children"], label.as_deref().or(container));
                continue;
            }
            let code = entry["type"]
                .as_i64()
                .or_else(|| entry["type"].as_str()?.parse().ok());
            let is = |names: &[&str], codes: &[i64]| {
                names.contains(&friendly.as_str()) || code.is_some_and(|c| codes.contains(&c))
            };
            let name = string(entry, "name").filter(|n| !n.is_empty());
            if is(WIDGETS, WIDGET_CODES) {
                self.widgets.extend(label.or(name));
                continue;
            }
            let (false, Some(name)) = (is(LAYOUT, LAYOUT_CODES), name) else {
                continue;
            };
            let kind = Kind::of(&friendly);
            let table = match kind {
                Kind::ListCollector => string(entry, "table"),
                Kind::Reference => string(entry, "reference"),
                _ => None,
            };
            let choices = entry["choices"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|choice| {
                    let value = scalar(&choice["value"]).filter(|v| !v.is_empty())?;
                    let label = string(choice, "label").unwrap_or_else(|| value.clone());
                    Some((value, label))
                })
                .collect();
            self.variables.push(Variable {
                label: label.unwrap_or_else(|| name.clone()),
                name,
                kind,
                mandatory: entry["mandatory"] == true,
                read_only: entry["read_only"] == true,
                default: scalar(&entry["value"]).filter(|v| !v.is_empty()),
                choices,
                table: table
                    .filter(|t| !t.is_empty())
                    .or_else(|| (friendly == "requested_for").then(|| "sys_user".into())),
                qualifier: string(entry, "ref_qualifier").filter(|q| !q.trim().is_empty()),
                container: container.map(str::to_owned),
                type_name: match (friendly.is_empty(), code) {
                    (false, _) => friendly,
                    (true, Some(code)) => format!("type {code}"),
                    (true, None) => "unknown".into(),
                },
            });
        }
    }

    pub fn variable(&self, name: &str) -> Option<&Variable> {
        self.variables.iter().find(|v| v.name == name)
    }

    pub fn portal_url(&self, base: &str) -> String {
        format!("{base}/sp?id=sc_cat_item&sys_id={}", self.sys_id)
    }

    /// Refuses items that an order from here would get wrong.
    pub fn check_orderable(&self, base: &str) -> Result<()> {
        let refuse = |why: String| {
            Err(Error::invalid(format!(
                "you asked for \"{}\", {why}; order it in the portal: {}",
                self.name,
                self.portal_url(base)
            )))
        };
        if !is_orderable_class(&self.class) {
            return refuse(format!(
                "which is a {} ({}) and not an item to order",
                kind_label(&self.class),
                self.class
            ));
        }
        // An optional set stays empty, as the portal sends it without rows.
        let set = self
            .variables
            .iter()
            .find(|v| v.kind == Kind::MultiRowSet && v.mandatory);
        if let Some(set) = set {
            return refuse(format!(
                "which needs rows in the multi-row variable set {}, and this command cannot fill one",
                set.name
            ));
        }
        match self.mandatory_attachment {
            true => refuse("which needs an attachment, and this command cannot add one".into()),
            false => Ok(()),
        }
    }

    pub fn to_json(&self, base: &str) -> Value {
        json!({
            "sys_id": self.sys_id,
            "name": self.name,
            "short_description": self.short_description,
            "class": self.class,
            "kind": kind_label(&self.class),
            "orderable": self.check_orderable(base).is_ok(),
            "instance_orderable": self.instance_orderable,
            "show_quantity": self.show_quantity,
            "client_logic": self.client_logic,
            "custom_widgets": self.widgets,
            "portal_url": self.portal_url(base),
            "variables": self.variables.iter().map(Variable::to_json).collect::<Vec<_>>(),
        })
    }

    /// Names that a query can reach as `variables.NAME`.
    pub fn readable_names(&self) -> Vec<&str> {
        self.variables
            .iter()
            .filter(|v| v.kind.has_value())
            .map(|v| v.name.as_str())
            .filter(|name| name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
            .collect()
    }

    /// What a mistyped name most likely meant: the same name or label in
    /// another case, or the only name within two edits.
    fn suggestion(&self, name: &str) -> String {
        let name = name.to_lowercase();
        let same = self
            .variables
            .iter()
            .find(|v| v.name.to_lowercase() == name || v.label.to_lowercase() == name);
        let near = || {
            // Two edits would make most short names alike.
            if name.chars().count() < 4 {
                return None;
            }
            let mut scored: Vec<(usize, &Variable)> = self
                .variables
                .iter()
                .map(|v| (edit_distance(&name, &v.name.to_lowercase()), v))
                .filter(|(distance, _)| *distance <= 2)
                .collect();
            scored.sort_by_key(|(distance, _)| *distance);
            match scored.as_slice() {
                [(_, only)] => Some(*only),
                [(best, first), (next, _), ..] if best < next => Some(*first),
                _ => None,
            }
        };
        match same.or_else(near) {
            Some(variable) => format!(" (did you mean `{}`?)", variable.name),
            None => String::new(),
        }
    }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, a) in left.chars().enumerate() {
        let mut current = vec![row + 1];
        for (column, b) in right.iter().enumerate() {
            let best = (previous[column] + usize::from(a != *b))
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
            current.push(best);
        }
        previous = current;
    }
    previous[right.len()]
}

/// A value for a variable and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub value: String,
    /// From `--var`: visible in the shell history and process lists.
    pub from_command_line: bool,
    /// A list, which only a list collector takes.
    pub list: bool,
}

pub type Inputs = BTreeMap<String, Input>;

/// `--var name=value`. The text is never repeated in an error: a mistyped
/// assignment may be a secret.
pub fn parse_assignment(text: &str) -> Result<(String, Input)> {
    let bad = || {
        Error::invalid(
            "you passed a --var without a name or without `=` (its text is not shown, because it may be a secret); use --var name=value",
        )
    };
    let (name, value) = text.split_once('=').ok_or_else(bad)?;
    if name.trim().is_empty() {
        return Err(bad());
    }
    let input = Input {
        value: value.to_owned(),
        from_command_line: true,
        list: false,
    };
    Ok((name.trim().to_owned(), input))
}

/// `--vars-file`: a YAML or JSON object of names to values or lists.
///
/// Every value is taken as the text written, so `01234`, `1.10` and `yes`
/// reach the instance unchanged. Parser messages can quote a secret, so they
/// are not shown.
pub fn parse_document(text: &str) -> Result<Inputs> {
    let bad = || {
        let json = text.trim_start().starts_with(['{', '[']);
        let at = match serde_json::from_str::<serde::de::IgnoredAny>(text) {
            Err(err) if json => format!(" (line {}, column {})", err.line(), err.column()),
            _ => String::new(),
        };
        Error::invalid(format!(
            "--vars-file is not an object of unique variable names to texts, numbers, booleans, or lists of those{at}; write YAML such as `name: value`, or JSON"
        ))
    };
    // The first pass learns the shape. It types values (`01234` would lose
    // its zero), so the second pass reads them as text.
    let shapes: BTreeMap<String, Shape> = serde_yaml_ng::from_str(text).map_err(|_| bad())?;
    let mut lists = BTreeSet::new();
    for (name, shape) in &shapes {
        match shape {
            Shape::Scalar { .. } => {}
            Shape::List { comma: true } => {
                return Err(Error::invalid(format!(
                    "in --vars-file an entry of the list {name} has a comma, which separates values; give each value its own entry"
                )));
            }
            Shape::List { comma: false } => {
                lists.insert(name.as_str());
            }
            Shape::Other => return Err(bad()),
        }
    }
    serde::de::DeserializeSeed::deserialize(
        TextValues(&lists),
        serde_yaml_ng::Deserializer::from_str(text),
    )
    .map_err(|_| bad())
}

/// What kind of value a variable has, whatever its type.
enum Shape {
    Scalar {
        comma: bool,
    },
    List {
        comma: bool,
    },
    /// Null, or nested.
    Other,
}

impl<'de> serde::Deserialize<'de> for Shape {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(AnyShape)
    }
}

struct AnyShape;

impl<'de> serde::de::Visitor<'de> for AnyShape {
    type Value = Shape;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a value")
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<Shape, E> {
        Ok(Shape::Scalar { comma: false })
    }

    fn visit_i64<E>(self, _: i64) -> std::result::Result<Shape, E> {
        Ok(Shape::Scalar { comma: false })
    }

    fn visit_u64<E>(self, _: u64) -> std::result::Result<Shape, E> {
        Ok(Shape::Scalar { comma: false })
    }

    fn visit_i128<E>(self, _: i128) -> std::result::Result<Shape, E> {
        Ok(Shape::Scalar { comma: false })
    }

    fn visit_u128<E>(self, _: u128) -> std::result::Result<Shape, E> {
        Ok(Shape::Scalar { comma: false })
    }

    fn visit_f64<E>(self, _: f64) -> std::result::Result<Shape, E> {
        Ok(Shape::Scalar { comma: false })
    }

    fn visit_str<E>(self, text: &str) -> std::result::Result<Shape, E> {
        Ok(Shape::Scalar {
            comma: text.contains(','),
        })
    }

    fn visit_unit<E>(self) -> std::result::Result<Shape, E> {
        Ok(Shape::Other)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Shape, A::Error> {
        let mut shape = Shape::List { comma: false };
        while let Some(item) = seq.next_element::<Shape>()? {
            shape = match (shape, item) {
                (Shape::List { comma }, Shape::Scalar { comma: own }) => Shape::List {
                    comma: comma || own,
                },
                _ => Shape::Other,
            };
        }
        Ok(shape)
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Shape, A::Error> {
        use serde::de::IgnoredAny;
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Shape::Other)
    }
}

/// Reads the object of `--vars-file` with every value as text.
struct TextValues<'a>(&'a BTreeSet<&'a str>);

impl<'de> serde::de::DeserializeSeed<'de> for TextValues<'_> {
    type Value = Inputs;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Inputs, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for TextValues<'_> {
    type Value = Inputs;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("an object of variable names to values")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Inputs, A::Error> {
        let mut out = Inputs::new();
        while let Some(name) = map.next_key::<String>()? {
            let list = self.0.contains(name.as_str());
            let value = match list {
                true => map.next_value::<Vec<String>>()?.join(","),
                false => map.next_value::<String>()?,
            };
            let input = Input {
                value,
                from_command_line: false,
                list,
            };
            if out.insert(name, input).is_some() {
                return Err(serde::de::Error::custom("duplicate variable name"));
            }
        }
        Ok(out)
    }
}

/// A reference that the instance must confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub variable: String,
    pub table: String,
    pub qualifier: Option<String>,
    pub values: Vec<String>,
}

impl Check {
    /// A script looks at the form, which a query cannot see.
    pub fn has_script(&self) -> bool {
        self.qualifier
            .as_ref()
            .is_some_and(|q| q.to_ascii_lowercase().contains("javascript:"))
    }
}

#[derive(Debug, Default)]
pub struct Validation {
    pub variables: BTreeMap<String, String>,
    pub checks: Vec<Check>,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    pub uses_dates: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Given,
    Default,
    /// The form itself sends the default of a read-only variable.
    ReadOnlyDefault,
}

/// Checks the values against the item's variables. Defaults fill variables
/// that were not given, as the portal sends them.
pub fn validate(item: &Item, inputs: &Inputs, quantity: u32, allow_unknown: bool) -> Validation {
    let mut out = Validation::default();
    if quantity == 0 {
        out.problems.push("--quantity must be at least 1".into());
    } else if quantity != 1 && item.show_quantity == Some(false) {
        out.problems.push(format!(
            "\"{}\" has no quantity; leave --quantity out",
            item.name
        ));
    }
    for name in inputs.keys().filter(|n| item.variable(n).is_none()) {
        let message = format!("unknown variable `{name}`{}", item.suggestion(name));
        match allow_unknown {
            true => out
                .warnings
                .push(format!("{message}; not sent (--allow-unknown-vars)")),
            false => out.problems.push(format!(
                "{message}; the instance would ignore it (see `sn catalog show`)"
            )),
        }
    }
    for variable in &item.variables {
        let before = out.problems.len();
        match (inputs.get(&variable.name), &variable.default) {
            (Some(input), _) => check_value(variable, input, Source::Given, &mut out),
            (None, _) if variable.kind == Kind::MultiRowSet => {
                let logic = match item.client_logic {
                    true => {
                        "; the item's client logic may need rows, which the instance does not check"
                    }
                    false => "",
                };
                out.warnings.push(format!(
                    "{}: optional multi-row variable set left empty{logic}",
                    variable.name
                ));
            }
            // A default can name a record or choice that is gone, so it is
            // checked like a value.
            (None, Some(default)) if variable.kind.has_value() => {
                let input = Input {
                    value: default.clone(),
                    from_command_line: false,
                    list: false,
                };
                let (source, note) = match variable.read_only {
                    true => (
                        Source::ReadOnlyDefault,
                        " (this is the item's read-only default; use the portal, or ask the item's owner to fix it)",
                    ),
                    false => (
                        Source::Default,
                        " (this is the item's default; pass a value to replace it)",
                    ),
                };
                check_value(variable, &input, source, &mut out);
                for problem in &mut out.problems[before..] {
                    problem.push_str(note);
                }
            }
            (None, _) => {}
        }
        let rejected = out.problems.len() > before;
        // A mandatory checkbox must be ticked, as on the portal.
        let filled = out
            .variables
            .get(&variable.name)
            .is_some_and(|value| match variable.kind {
                Kind::CheckBox => value == "true",
                _ => !value.trim().is_empty(),
            });
        if variable.mandatory && !variable.read_only && !rejected && !filled {
            out.problems
                .push(format!("{}: required ({})", variable.name, variable.label));
        }
    }
    out
}

fn check_value(variable: &Variable, input: &Input, source: Source, out: &mut Validation) {
    let name = &variable.name;
    let given = input.value.as_str();
    let refused = if given == REDACTED {
        Some(format!(
            "the value is the placeholder {REDACTED}; pass the real value"
        ))
    } else if variable.read_only && source != Source::ReadOnlyDefault {
        Some("read-only; the form does not accept a value".to_owned())
    } else if input.from_command_line && variable.is_sensitive() {
        Some("it looks like a secret, and --var shows values in the shell history and process lists; pass it with --vars-file FILE, or --vars-file - with --yes".to_owned())
    } else if input.list && variable.kind != Kind::ListCollector {
        Some(format!(
            "you passed a list, but a {} takes one value",
            variable.type_name
        ))
    } else {
        None
    };
    if let Some(why) = refused {
        out.problems.push(format!("{name}: {why}"));
        return;
    }
    if given.trim().is_empty() {
        // Blank text is kept; other kinds have no blank form.
        let blank = match variable.kind {
            Kind::Text | Kind::MultiLineText | Kind::Masked => given,
            _ => "",
        };
        out.variables.insert(name.clone(), blank.to_owned());
        return;
    }
    let trimmed = given.trim();
    let checked = match variable.kind {
        Kind::Text | Kind::MultiLineText | Kind::Masked => Ok(given.to_owned()),
        Kind::CheckBox => flag(trimmed).map(|on| on.to_string()),
        Kind::YesNo => flag(trimmed).map(|on| if on { "Yes" } else { "No" }.to_owned()),
        Kind::Choice if variable.choices.is_empty() => {
            out.warnings.push(format!(
                "{name}: the instance does not publish the choices of this {}; sent unchecked",
                variable.type_name
            ));
            Ok(given.to_owned())
        }
        Kind::Choice => choice(&variable.choices, given),
        Kind::Reference | Kind::ListCollector => {
            let values: Vec<String> = match variable.kind {
                Kind::ListCollector => given
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
                    .collect(),
                _ => vec![trimmed.to_owned()],
            };
            let joined = values.join(",");
            match &variable.table {
                Some(table) => out.checks.push(Check {
                    variable: name.clone(),
                    table: table.clone(),
                    qualifier: variable.qualifier.clone(),
                    values,
                }),
                None => out.warnings.push(format!(
                    "{name}: the instance does not say which table this refers to; sent unchecked"
                )),
            }
            Ok(joined)
        }
        Kind::Date => {
            out.uses_dates = true;
            match is_date(trimmed) {
                true => Ok(trimmed.to_owned()),
                false => Err(format!("\"{trimmed}\" is not a date; use {DATE_FORMAT}")),
            }
        }
        Kind::DateTime => {
            out.uses_dates = true;
            date_time(trimmed).map(|()| trimmed.to_owned())
        }
        Kind::Attachment => {
            Err("attachment variables are not supported; use the portal".to_owned())
        }
        Kind::MultiRowSet => Err(
            "this command cannot fill multi-row variable sets; leave it out, or use the portal"
                .to_owned(),
        ),
        Kind::Unknown => {
            out.warnings.push(format!(
                "{name}: the variable type `{}` is not known; sent unchecked",
                variable.type_name
            ));
            Ok(given.to_owned())
        }
    };
    match checked {
        Ok(value) => {
            out.variables.insert(name.clone(), value);
        }
        Err(why) => out.problems.push(format!("{name}: {why}")),
    }
}

fn flag(text: &str) -> std::result::Result<bool, String> {
    match text.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        _ => Err(format!("\"{text}\" is not a boolean; use true or false")),
    }
}

fn choice(choices: &[(String, String)], given: &str) -> std::result::Result<String, String> {
    if choices.iter().any(|(value, _)| value == given) {
        return Ok(given.to_owned());
    }
    let wanted = given.trim().to_lowercase();
    let matching: Vec<&str> = choices
        .iter()
        .filter(|(value, label)| value.to_lowercase() == wanted || label.to_lowercase() == wanted)
        .map(|(value, _)| value.as_str())
        .collect();
    match matching.as_slice() {
        [value] => Ok((*value).to_owned()),
        _ => {
            let all: Vec<String> = choices
                .iter()
                .map(|(value, label)| match value == label {
                    true => value.clone(),
                    false => format!("{value} ({label})"),
                })
                .collect();
            Err(format!(
                "\"{given}\" is not one of the choices: {}",
                all.join(", ")
            ))
        }
    }
}

/// True when `text` has digits where the pattern has 0, and the pattern's
/// other characters elsewhere.
fn fits(text: &str, pattern: &str) -> bool {
    text.len() == pattern.len()
        && text
            .bytes()
            .zip(pattern.bytes())
            .all(|(have, want)| match want {
                b'0' => have.is_ascii_digit(),
                _ => have == want,
            })
}

fn is_date(text: &str) -> bool {
    if !fits(text, "0000-00-00") {
        return false;
    }
    let number = |from: usize, to: usize| text[from..to].parse::<u32>().unwrap_or_default();
    let (year, month, day) = (number(0, 4), number(5, 7), number(8, 10));
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    (1..=days).contains(&day)
}

fn date_time(text: &str) -> std::result::Result<(), String> {
    let has_offset = text
        .get(19..)
        .is_some_and(|rest| rest.starts_with(['+', '-']));
    if text.contains('T') || text.ends_with('Z') || has_offset {
        return Err(format!(
            "\"{text}\" is an ISO 8601 time, which the instance misreads; use {DATE_FORMAT} {TIME_FORMAT} in your ServiceNow time zone"
        ));
    }
    let valid = fits(text, "0000-00-00 00:00:00")
        && is_date(&text[..10])
        && &text[11..13] < "24"
        && &text[14..16] < "60"
        && &text[17..19] < "60";
    match valid {
        true => Ok(()),
        false => Err(format!(
            "\"{text}\" is not a date and time; use {DATE_FORMAT} {TIME_FORMAT}"
        )),
    }
}

/// A variable value read from a requested item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ordered {
    pub name: String,
    pub label: String,
    pub value: String,
    pub display: String,
    pub sensitive: bool,
    pub read_only: bool,
    /// An empty value was cleared on purpose when there is a default.
    pub has_default: bool,
    /// The form that an order accepts, when the answer has it.
    pub order_value: Option<String>,
}

/// The values of a requested item read in both display modes. A variable
/// that the answer leaves out is skipped, never reported as empty.
pub fn ordered(item: &Item, record: &Value) -> Vec<Ordered> {
    item.variables
        .iter()
        .filter_map(|variable| {
            let cell = record.get(format!("variables.{}", variable.name))?;
            let (value, display) = match cell {
                Value::Object(_) => (scalar(&cell["value"]), scalar(&cell["display_value"])),
                other => (scalar(other), None),
            };
            let value = value?;
            Some(Ordered {
                name: variable.name.clone(),
                label: variable.label.clone(),
                // Times are stored in UTC and ordered in the user's time
                // zone, which only the display value has.
                order_value: match variable.kind {
                    Kind::DateTime if !value.is_empty() => display.clone(),
                    _ => Some(value.clone()),
                },
                display: display.unwrap_or_else(|| value.clone()),
                value,
                sensitive: variable.is_sensitive(),
                read_only: variable.read_only,
                has_default: variable.default.is_some(),
            })
        })
        .collect()
}

/// What an order answer says happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Request {
        sys_id: String,
        number: Option<String>,
    },
    /// Nothing was ordered: the item is in this cart.
    CartOnly { cart_id: String },
    /// The names of the answer's fields; its values could echo variables.
    Unknown {
        keys: Vec<String>,
        number: Option<String>,
    },
}

impl Outcome {
    pub fn of(answer: &Value) -> Self {
        let id = |field: &str| {
            answer[field]
                .as_str()
                .filter(|id| is_sys_id(id))
                .map(str::to_owned)
        };
        // Only a value that looks like a number is ever shown.
        let number = |field: &str| {
            answer[field]
                .as_str()
                .map(str::trim)
                .filter(|n| {
                    (1..=40).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_alphanumeric())
                })
                .map(str::to_owned)
        };
        // A bare sys_id can be a cart or a cart item.
        let names_request = match answer["table"].as_str() {
            Some(table) => table == "sc_request",
            None => answer.get("cart_id").is_none(),
        };
        if let Some(sys_id) = id("request_id").or_else(|| id("sys_id").filter(|_| names_request)) {
            let number = number("request_number").or_else(|| number("number"));
            return Self::Request { sys_id, number };
        }
        if let Some(cart_id) = id("cart_id") {
            return Self::CartOnly { cart_id };
        }
        let mut keys: Vec<String> = answer
            .as_object()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default();
        keys.sort();
        Self::Unknown {
            keys,
            number: number("request_number"),
        }
    }
}

/// The caller's cart, the one that the portal shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Cart {
    pub id: String,
    pub items: Vec<CartItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CartItem {
    pub id: String,
    pub name: String,
    /// The catalog item that this is a copy of.
    pub catalog_item: Option<String>,
    pub can_order: Option<bool>,
    /// As the instance described it, with the price `frequency` it was
    /// grouped under.
    pub details: Value,
}

impl Cart {
    /// `GET /cart` groups items by price frequency; `add_to_cart` lists them
    /// at the top.
    pub fn parse(answer: &Value) -> Result<Self> {
        let id = answer["cart_id"]
            .as_str()
            .filter(|id| is_sys_id(id))
            .ok_or_else(|| {
                Error::api(
                    "the instance described the cart without its id",
                    Value::Null,
                )
            })?;
        let mut items: Vec<CartItem> = Vec::new();
        let groups = answer
            .as_object()
            .into_iter()
            .flatten()
            .map(|(frequency, group)| (frequency.as_str(), &group["items"]))
            .chain([("none", &answer["items"])]);
        for (frequency, group) in groups {
            for entry in group.as_array().into_iter().flatten() {
                let item = CartItem::parse(entry, frequency)?;
                // An item listed twice is one item.
                if !items.iter().any(|have| have.id == item.id) {
                    items.push(item);
                }
            }
        }
        Ok(Self {
            id: id.to_owned(),
            items,
        })
    }

    pub fn to_json(&self) -> Value {
        let items: Vec<&Value> = self.items.iter().map(|i| &i.details).collect();
        json!({ "cart_id": self.id, "items": items })
    }

    pub fn item(&self, id: &str) -> Option<&CartItem> {
        self.items.iter().find(|i| i.id.eq_ignore_ascii_case(id))
    }

    pub fn names(&self) -> String {
        let names: Vec<&str> = self.items.iter().map(|i| i.name.as_str()).collect();
        names.join(", ")
    }

    /// What must not change between a confirmation and the write.
    pub fn contents(&self) -> (&str, BTreeSet<(&str, String)>) {
        let items = self
            .items
            .iter()
            .map(|i| {
                let quantity = scalar(&i.details["quantity"]).unwrap_or_default();
                (i.id.as_str(), quantity.trim().to_owned())
            })
            .collect();
        (&self.id, items)
    }
}

impl CartItem {
    fn parse(entry: &Value, frequency: &str) -> Result<Self> {
        let id = |field: &str| {
            entry[field]
                .as_str()
                .filter(|id| is_sys_id(id))
                .map(str::to_ascii_lowercase)
        };
        let name = ["item_name", "name"]
            .iter()
            .find_map(|f| entry[*f].as_str())
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("(unnamed item)");
        let own = id("cart_item_id").ok_or_else(|| {
            Error::api(
                "the instance listed an item of the cart without its id",
                Value::Null,
            )
        })?;
        let mut details = entry.clone();
        details["frequency"] = frequency.into();
        Ok(Self {
            id: own,
            name: name.to_owned(),
            catalog_item: id("catalog_item_id"),
            can_order: entry["can_order"].as_bool(),
            details,
        })
    }
}

/// A checked order.
#[derive(Debug, Clone)]
pub struct Plan {
    pub item_id: String,
    pub item_name: String,
    pub item_class: String,
    pub quantity: u32,
    pub requested_for: Option<String>,
    pub variables: BTreeMap<String, String>,
    pub sensitive: BTreeSet<String>,
    pub warnings: Vec<String>,
}

impl Plan {
    /// The values without secrets, for display.
    pub fn shown(&self) -> Map<String, Value> {
        self.variables
            .iter()
            .map(|(name, value)| {
                let shown = match self.sensitive.contains(name) && !value.is_empty() {
                    true => REDACTED,
                    false => value.as_str(),
                };
                (name.clone(), shown.into())
            })
            .collect()
    }

    /// Submitted secrets, the longest first, to take out of error messages.
    pub fn secrets(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .sensitive
            .iter()
            .filter_map(|name| self.variables.get(name))
            .filter(|value| !value.is_empty())
            .cloned()
            .collect();
        out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        out.dedup();
        out
    }

    pub fn to_json(&self) -> Value {
        json!({
            "dry_run": true,
            "operation": "order",
            "item": { "sys_id": self.item_id, "name": self.item_name, "class": self.item_class },
            "quantity": self.quantity,
            "requested_for": self.requested_for,
            "variables": self.shown(),
            "warnings": self.warnings,
        })
    }

    /// The request body, with the real values.
    pub fn body(&self) -> Value {
        let mut body = json!({
            "sysparm_quantity": self.quantity.to_string(),
            "variables": self.variables,
        });
        if let Some(user) = &self.requested_for {
            body["sysparm_requested_for"] = user.as_str().into();
        }
        body
    }
}

#[cfg(test)]
mod tests;
