//! The state of the workspace.

use crate::input::Editor;
use crate::keys::Act;
use crate::model::{Baseline, Cell, Field, Kind, Row};
use crate::msg::{Attachment, Candidate, Change, Entry, Fault, Req, Sla, Write};
use crate::query::{Condition, Order};
use crate::tables::{Column, Related};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Something that comes from the instance.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Load<T> {
    #[default]
    Idle,
    Loading(Req),
    Ready(T),
    Failed(Fault),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading(_))
    }

    pub fn waits_for(&self, req: Req) -> bool {
        matches!(self, Self::Loading(r) if *r == req)
    }

    pub fn set(&mut self, outcome: Result<T, Fault>) {
        *self = match outcome {
            Ok(value) => Self::Ready(value),
            Err(fault) => Self::Failed(fault),
        };
    }

    /// Takes the answer it waits for. False for any other.
    pub fn answer(&mut self, req: Req, outcome: Result<T, Fault>) -> bool {
        let awaited = self.waits_for(req);
        if awaited {
            self.set(outcome);
        }
        awaited
    }

    /// An answer that will not come any more, or that the end of a session
    /// refused.
    pub fn forget(&mut self) {
        if self.is_loading() || matches!(self, Self::Failed(fault) if fault.code == "auth") {
            *self = Self::Idle;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileInfo {
    pub name: String,
    pub host: String,
    pub read_only: bool,
}

/// The profile in use.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Connection {
    pub profile: String,
    /// `https://acme.service-now.com`; empty before the first sign-in.
    pub base: String,
    pub host: String,
    pub read_only: bool,
    /// Writes are shown and not sent.
    pub dry_run: bool,
    /// Why there is no session, when there is none.
    pub signed_out: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub page_size: u64,
    /// Where downloads go.
    pub download_dir: PathBuf,
    /// The table, query and view to start with.
    pub start: Option<Start>,
    /// Pinned tables.
    pub pins: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Start {
    pub table: String,
    pub query: Option<String>,
    pub view: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Rail,
    List,
    Record,
}

/// A line of the left rail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RailItem {
    Group(&'static str),
    /// Id, label.
    Section(String, String),
}

/// Why a list has no rows yet.
#[derive(Clone, Debug, PartialEq)]
pub enum ListPhase {
    /// Needs the user or the fields of the table first.
    Waiting,
    Loading(Req),
    Ready,
    Failed(Fault),
}

/// A request beside the rows on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Extra {
    #[default]
    None,
    /// The rows after the last one.
    More(Req),
    /// The same rows again.
    Refresh { req: Req, limit: u64 },
}

#[derive(Clone, Debug)]
pub struct ListState {
    /// The section, or `pin:TABLE`, or `None` for another table.
    pub section: Option<String>,
    pub table: String,
    pub title: String,
    /// What the list started with, to go back to.
    pub base: (String, Order),
    pub query: String,
    pub order: Order,
    pub view: Option<String>,
    pub columns: Vec<Column>,
    pub phase: ListPhase,
    pub extra: Extra,
    pub rows: Vec<Row>,
    pub total: Option<u64>,
    /// Where the next page starts. Access rules hide rows after the limit
    /// applies, so this is not the number of rows.
    pub next_offset: u64,
    pub exhausted: bool,
    /// A place in the rows that match the search.
    pub cursor: usize,
    pub selected: BTreeSet<String>,
    pub search: Editor,
    pub searching: bool,
}

impl ListState {
    pub fn new(table: &str, title: &str, query: &str, order: Order) -> Self {
        Self {
            section: None,
            table: table.to_owned(),
            title: title.to_owned(),
            base: (query.to_owned(), order.clone()),
            query: query.to_owned(),
            order,
            view: None,
            columns: crate::tables::columns(table).unwrap_or_default(),
            phase: ListPhase::Waiting,
            extra: Extra::None,
            rows: Vec::new(),
            total: None,
            next_offset: 0,
            exhausted: false,
            cursor: 0,
            selected: BTreeSet::new(),
            search: Editor::default(),
            searching: false,
        }
    }

    /// The rows that match the search, as places in `rows`.
    pub fn visible(&self) -> Vec<usize> {
        self.matching().collect()
    }

    fn matching(&self) -> impl Iterator<Item = usize> {
        let words: Vec<String> = self
            .search
            .text
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        (0..self.rows.len()).filter(move |i| self.rows[*i].matches(&words))
    }

    pub fn current(&self) -> Option<&Row> {
        self.matching().nth(self.cursor).map(|i| &self.rows[i])
    }

    pub fn is_changed(&self) -> bool {
        (&self.query, &self.order) != (&self.base.0, &self.base.1)
    }

    pub fn is_filtered(&self) -> bool {
        self.query != self.base.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Details,
    Activity,
    Related,
    Attachments,
    Slas,
    History,
}

impl Tab {
    pub const ALL: [Self; 6] = [
        Self::Details,
        Self::Activity,
        Self::Related,
        Self::Attachments,
        Self::Slas,
        Self::History,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Details => "Details",
            Self::Activity => "Activity",
            Self::Related => "Related",
            Self::Attachments => "Files",
            Self::Slas => "SLAs",
            Self::History => "History",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

/// A change that is not sent yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub field: String,
    pub label: String,
    pub old: Cell,
    pub new: Cell,
}

/// A field edited in its own row.
#[derive(Clone, Debug)]
pub struct Inline {
    pub field: Field,
    pub editor: Editor,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RecordState {
    pub table: String,
    /// Empty for a record that does not exist yet.
    pub sys_id: String,
    pub row: Row,
    /// The whole record; the row of the list stands in until then.
    pub full: Load<()>,
    pub baseline: Baseline,
    pub tab: Tab,
    pub cursor: usize,
    pub related_cursor: usize,
    pub file_cursor: usize,
    /// Lines scrolled in Activity, SLAs and History.
    pub scroll: usize,
    pub show_empty: bool,
    pub edits: Vec<Edit>,
    pub inline: Option<Inline>,
    pub activity: Load<Vec<Entry>>,
    pub related: Vec<(Related, Load<Vec<Row>>)>,
    pub attachments: Load<Vec<Attachment>>,
    pub slas: Load<Vec<Sla>>,
    pub history: Load<Vec<Change>>,
}

impl RecordState {
    pub fn new(table: &str, sys_id: &str, row: Row) -> Self {
        Self {
            table: table.to_owned(),
            sys_id: sys_id.to_owned(),
            baseline: row.baseline(),
            row,
            full: Load::Idle,
            tab: Tab::Details,
            cursor: 0,
            related_cursor: 0,
            file_cursor: 0,
            scroll: 0,
            show_empty: false,
            edits: Vec::new(),
            inline: None,
            activity: Load::Idle,
            related: Vec::new(),
            attachments: Load::Idle,
            slas: Load::Idle,
            history: Load::Idle,
        }
    }

    pub fn is_new(&self) -> bool {
        self.sys_id.is_empty()
    }

    pub fn name(&self) -> String {
        match self.is_new() {
            true => format!("New {}", crate::tables::one(&self.table)),
            false => {
                let name = self.row.name();
                if name.is_empty() {
                    self.sys_id.clone()
                } else {
                    name
                }
            }
        }
    }

    /// The value with the change that waits.
    pub fn cell(&self, field: &str) -> &Cell {
        self.edits
            .iter()
            .find(|e| e.field == field)
            .map_or_else(|| self.row.cell(field), |e| &e.new)
    }

    pub fn edit(&self, field: &str) -> Option<&Edit> {
        self.edits.iter().find(|e| e.field == field)
    }

    /// Something typed or changed here is not sent yet.
    pub fn has_draft(&self) -> bool {
        !self.edits.is_empty()
            || self
                .inline
                .as_ref()
                .is_some_and(|i| i.editor.text != self.cell(&i.field.name).value)
    }

    /// Records a change, or forgets it when it puts the old value back.
    pub fn set_edit(&mut self, field: &Field, new: Cell) {
        let old = self.row.cell(&field.name).clone();
        self.edits.retain(|e| e.field != field.name);
        if new.value != old.value {
            self.edits.push(Edit {
                field: field.name.clone(),
                label: field.label.clone(),
                old,
                new,
            });
        }
    }

    /// The rows of the Related tab that can be opened: list, row.
    pub fn related_rows(&self) -> Vec<(usize, usize)> {
        self.related
            .iter()
            .enumerate()
            .flat_map(|(i, (_, load))| {
                let n = load.ready().map_or(0, Vec::len);
                (0..n).map(move |j| (i, j))
            })
            .collect()
    }

    /// Requests that were on their way when the record was put aside.
    pub fn forget(&mut self) {
        self.full.forget();
        self.activity.forget();
        self.attachments.forget();
        self.slas.forget();
        self.history.forget();
        for (_, load) in &mut self.related {
            load.forget();
        }
        if self.related.iter().any(|(_, l)| matches!(l, Load::Idle)) {
            self.related.clear();
        }
    }
}

/// A place to come back to.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub list: ListState,
    pub record: Option<RecordState>,
    pub records: Vec<RecordState>,
    pub focus: Pane,
}

impl Snapshot {
    pub fn has_draft(&self) -> bool {
        self.record
            .iter()
            .chain(&self.records)
            .any(RecordState::has_draft)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Good,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    /// When it goes away.
    pub until: u64,
}

/// A record an action works on.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub table: String,
    pub sys_id: String,
    pub name: String,
    pub row: Row,
    pub baseline: Baseline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormKind {
    Comment,
    WorkNote,
    Assign,
    Resolve,
    Approve,
    Reject,
}

#[derive(Clone, Debug)]
pub struct FormField {
    pub field: Field,
    pub hint: &'static str,
    pub required: bool,
    pub cell: Cell,
    pub editor: Editor,
}

#[derive(Clone, Debug)]
pub struct Form {
    pub kind: FormKind,
    pub title: String,
    pub targets: Vec<Target>,
    pub fields: Vec<FormField>,
    pub cursor: usize,
    pub error: Option<String>,
}

/// What a picked value is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickFor {
    Record,
    Form(usize),
    FilterField(usize),
    FilterOp(usize),
    FilterValue(usize),
}

#[derive(Clone, Debug)]
pub enum PickSource {
    /// Everything is here already.
    Fixed(Vec<Candidate>),
    /// The instance searches this table.
    Table(String),
}

#[derive(Clone, Debug)]
pub struct Picker {
    pub title: String,
    pub purpose: PickFor,
    /// The field the value is for.
    pub field: Field,
    pub source: PickSource,
    pub editor: Editor,
    pub cursor: usize,
    pub found: Load<Vec<Candidate>>,
    /// When to ask the instance, after the typing stops.
    pub search_at: Option<u64>,
    /// Offers "(none)".
    pub can_clear: bool,
    /// Offers the signed-in user first.
    pub me: Option<Candidate>,
}

impl Picker {
    pub fn items(&self) -> Vec<Candidate> {
        let needle = self.editor.text.trim();
        let mut out = Vec::new();
        if needle.is_empty() {
            out.extend(self.me.clone());
        }
        match &self.source {
            PickSource::Fixed(all) if needle.is_empty() => out.extend(all.iter().cloned()),
            PickSource::Fixed(all) => {
                let mut scored: Vec<(i32, &Candidate)> = all
                    .iter()
                    .filter_map(|c| {
                        let score = crate::text::fuzzy(needle, &c.label)
                            .max(crate::text::fuzzy(needle, &c.detail))?;
                        Some((score, c))
                    })
                    .collect();
                scored.sort_by_key(|(score, _)| -score);
                out.extend(scored.into_iter().map(|(_, c)| c.clone()));
            }
            PickSource::Table(_) => {
                let me = self.me.as_ref().map(|c| c.value.as_str());
                out.extend(
                    self.found
                        .ready()
                        .into_iter()
                        .flatten()
                        .filter(|c| !needle.is_empty() || Some(c.value.as_str()) != me)
                        .cloned(),
                );
            }
        }
        if self.can_clear && needle.is_empty() {
            out.push(Candidate {
                value: String::new(),
                label: "(none)".into(),
                detail: "Leave the field empty".into(),
            });
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct FilterBuilder {
    pub table: String,
    pub conditions: Vec<Condition>,
    /// A condition, or one past the last for "add".
    pub row: usize,
    /// 0 field, 1 operator, 2 value.
    pub column: usize,
    /// The value typed in its place.
    pub editing: Option<Editor>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteDo {
    Act(Act),
    Section(String),
    Table(String, String),
    Record(String, String),
    /// A number or a link.
    Locate(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteItem {
    pub group: &'static str,
    pub title: String,
    pub detail: String,
    pub action: PaletteDo,
}

#[derive(Clone, Debug, Default)]
pub struct Palette {
    pub editor: Editor,
    pub cursor: usize,
    pub tables_only: bool,
    pub search_at: Option<u64>,
    pub tables: Load<Vec<(String, String)>>,
    pub records: Load<Vec<Row>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewLine {
    pub field: String,
    pub label: String,
    pub old: String,
    pub new: String,
    /// The value adds to the record and replaces nothing.
    pub adds: bool,
    /// Someone else changed this field meanwhile.
    pub moved: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReviewWork {
    Write(Vec<Write>),
    Create(String, Map<String, Value>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReviewState {
    Ready,
    Sending(Req, usize),
    /// One line for each problem.
    Failed(Vec<String>),
    /// Who changed the record, and when, in words.
    Conflict(String, String),
}

/// The screen before every write.
#[derive(Clone, Debug, PartialEq)]
pub struct Review {
    pub title: String,
    /// What the button says, and the toast after: "Assign", "assigned".
    pub verb: &'static str,
    pub done: &'static str,
    pub names: Vec<String>,
    pub lines: Vec<ReviewLine>,
    pub work: ReviewWork,
    pub state: ReviewState,
    pub scroll: usize,
}

/// What was asked for and waits for an answer of the instance. The next key
/// or click replaces it, so a late answer never acts on something else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pending {
    Locate(Req),
    /// An action that needs the fields of this table.
    Fields(String, Act),
}

/// What waits for a confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Then {
    Quit,
    CloseRecord,
    DropEdits,
    SwitchProfile(String),
}

#[derive(Clone, Debug)]
pub enum Overlay {
    Help(usize),
    Goto,
    Palette(Palette),
    Menu {
        items: Vec<Act>,
        cursor: usize,
    },
    Profiles(usize),
    Sort(usize),
    Query(Editor),
    Filter(FilterBuilder),
    Form(Form),
    Picker(Box<Picker>),
    Text {
        field: Field,
        editor: Editor,
    },
    /// A long text to read.
    Read {
        title: String,
        text: String,
        scroll: usize,
    },
    Review(Review),
    Confirm {
        text: String,
        yes: &'static str,
        then: Then,
    },
}

impl FormField {
    pub fn new(field: Field, hint: &'static str, required: bool) -> Self {
        let editor = match field.kind {
            Kind::Long | Kind::Journal => Editor::multiline(""),
            _ => Editor::default(),
        };
        Self {
            field,
            hint,
            required,
            cell: Cell::default(),
            editor,
        }
    }

    pub fn is_typed(&self) -> bool {
        !matches!(self.field.kind, Kind::Choice | Kind::Reference | Kind::Bool)
    }

    pub fn value(&self) -> Cell {
        match self.is_typed() {
            true => {
                let text = self.editor.text.trim().to_owned();
                Cell {
                    value: text.clone(),
                    display: text,
                }
            }
            false => self.cell.clone(),
        }
    }
}
