//! Everything that changes records: forms, the review before a write, and
//! what the instance answered. Nothing is sent before the review.

use super::App;
use super::state::*;
use crate::model::{self, Cell, Field, Kind, Row};
use crate::msg::{Call, Fault, Outcome, Req, Write, Written};
use crate::tables;
use crate::text;
use serde_json::{Map, Value};

/// Closing states of tables whose fields cannot be read.
const CLOSING: &[(&str, &[(&str, &str)])] = &[
    ("incident", &[("6", "Resolved"), ("7", "Closed")]),
    ("change_request", &[("3", "Closed"), ("4", "Canceled")]),
    (
        "sc_task",
        &[
            ("3", "Closed Complete"),
            ("4", "Closed Incomplete"),
            ("7", "Closed Skipped"),
        ],
    ),
];

impl App {
    pub(super) fn read_only_text(&self) -> String {
        format!(
            "Profile {} is read-only, so nothing can be changed. Press P to switch to another profile.",
            self.connection.profile
        )
    }

    /// The records an action works on, with their tables: the selected rows,
    /// the open record, or the row under the cursor.
    pub fn target_rows(&self) -> Vec<(&str, &Row)> {
        match &self.record {
            Some(record) if self.focus == Pane::Record => match record.is_new() {
                true => Vec::new(),
                false => vec![(&record.table, &record.row)],
            },
            _ if self.list.selected.is_empty() => {
                let current = self.list.current();
                current.map(|r| (r.table.as_str(), r)).into_iter().collect()
            }
            _ => self
                .list
                .rows
                .iter()
                .filter(|r| self.list.selected.contains(&r.sys_id))
                .map(|r| (r.table.as_str(), r))
                .collect(),
        }
    }

    pub fn targets(&self) -> Vec<Target> {
        match &self.record {
            Some(record) if self.focus == Pane::Record && !record.is_new() => {
                vec![record_target(record)]
            }
            _ => self
                .target_rows()
                .into_iter()
                .map(|(_, row)| target(row))
                .collect(),
        }
    }

    /// The approval of mine in the Related tab that an approval action
    /// decides; `None` when the targets are the approvals.
    fn approval(&self, targets: &[(&str, &Row)]) -> Result<Option<&Row>, String> {
        let open = |r: &Row| r.value("state") == "requested";
        if !targets.is_empty() && targets.iter().all(|(t, _)| *t == "sysapproval_approver") {
            return match targets.iter().all(|(_, row)| open(row)) {
                true => Ok(None),
                false => Err("Only an approval in the state Requested can be decided.".into()),
            };
        }
        let mine = self.me().map(|m| m.sys_id.as_str()).unwrap_or_default();
        let found = match (self.focus, &self.record) {
            (Pane::Record, Some(record)) => record
                .related
                .iter()
                .filter(|(list, _)| list.table == "sysapproval_approver")
                .filter_map(|(_, load)| load.ready())
                .flatten()
                .find(|r| r.value("approver") == mine && open(r)),
            _ => None,
        };
        match found {
            Some(row) => Ok(Some(row)),
            None => Err("No approval of yours waits here. Press g a for your approvals.".into()),
        }
    }

    /// True when the table has comments and work notes.
    fn has_journal(&self, table: &str, field: &str) -> bool {
        match self.meta(table) {
            Some(meta) => meta.field(field).is_some(),
            None => tables::is_task(table),
        }
    }

    /// Why a form cannot open here, or `None` when it can.
    pub fn form_problem(&self, kind: FormKind, targets: &[(&str, &Row)]) -> Option<String> {
        if self.connection.read_only {
            return Some(self.read_only_text());
        }
        if matches!(kind, FormKind::Approve | FormKind::Reject) {
            return self.approval(targets).err();
        }
        let Some((first, _)) = targets.first() else {
            return Some("Select a record first.".into());
        };
        let all = |field: &str| {
            targets
                .chunk_by(|a, b| a.0 == b.0)
                .all(|same| self.has_journal(same[0].0, field))
        };
        match kind {
            FormKind::Comment if !all("comments") => {
                Some(format!("{} has no comments.", tables::many(first)))
            }
            FormKind::WorkNote if !all("work_notes") => {
                Some(format!("{} has no work notes.", tables::many(first)))
            }
            FormKind::Assign if !all("assigned_to") => {
                Some(format!("{} has no assignee.", tables::many(first)))
            }
            FormKind::Resolve if targets.iter().any(|(t, _)| t != first) => {
                Some("The selected records are of different kinds. Select one kind to close them together.".into())
            }
            FormKind::Resolve if !all("work_notes") => {
                Some(format!("{} is not closed like a task.", tables::many(first)))
            }
            _ => None,
        }
    }

    pub(super) fn open_form(&mut self, kind: FormKind) {
        if let Some(problem) = self.form_problem(kind, &self.target_rows()) {
            self.toast(ToastKind::Warning, problem);
            return;
        }
        let targets = match kind {
            FormKind::Approve | FormKind::Reject => match self.approval(&self.target_rows()) {
                Ok(Some(row)) => vec![target(row)],
                _ => self.targets(),
            },
            _ => self.targets(),
        };
        let Some(first) = targets.first() else { return };
        let table = first.table.clone();
        let names = match targets.len() {
            1 => first.name.clone(),
            n => format!("{n} records"),
        };
        let mut journal = Field::guess("work_notes");
        let (title, fields) = match kind {
            FormKind::Comment => {
                journal = Field::guess("comments");
                journal.label = "Comment, visible to the caller".into();
                (
                    format!("Comment on {names}"),
                    vec![FormField::new(journal, "", true)],
                )
            }
            FormKind::WorkNote => {
                journal.label = "Work note, for your colleagues only".into();
                (
                    format!("Work note on {names}"),
                    vec![FormField::new(journal, "", true)],
                )
            }
            FormKind::Assign => {
                let group = Field {
                    label: "Group".into(),
                    ..Field::guess("assignment_group")
                };
                let person = Field {
                    label: "Person".into(),
                    ..Field::guess("assigned_to")
                };
                journal.label = "Work note".into();
                let mut fields = vec![
                    FormField::new(group, "Keeps the group when empty", false),
                    FormField::new(person, "Keeps the person when empty", false),
                    FormField::new(journal, "Optional", false),
                ];
                for field in &mut fields[..2] {
                    field.cell = same_cell(&targets, &field.field.name)
                        .cloned()
                        .unwrap_or_default();
                }
                (format!("Assign {names}"), fields)
            }
            FormKind::Resolve => match self.resolve_fields(&table) {
                Some(fields) => (format!("Close {names}"), fields),
                None => return,
            },
            FormKind::Approve | FormKind::Reject => {
                let comments = Field {
                    label: match kind {
                        FormKind::Reject => "Reason".into(),
                        _ => "Comment".into(),
                    },
                    ..Field::guess("comments")
                };
                let approve = kind == FormKind::Approve;
                (
                    format!("{} {names}", if approve { "Approve" } else { "Reject" }),
                    vec![FormField::new(
                        comments,
                        if approve { "Optional" } else { "" },
                        !approve,
                    )],
                )
            }
        };
        self.overlays.push(Overlay::Form(Form {
            kind,
            title,
            targets,
            fields,
            cursor: 0,
            error: None,
        }));
    }

    /// The fields that closing a record of the table needs. `None` while the
    /// fields of the table load, or when nothing closes it.
    fn resolve_fields(&mut self, table: &str) -> Option<Vec<FormField>> {
        match self.metas.get(table) {
            Some(Load::Ready(_) | Load::Failed(_)) => {}
            _ => {
                self.want_meta(table);
                self.pending = Some(Pending::Fields(table.to_owned(), crate::keys::Act::Resolve));
                return None;
            }
        }
        let meta = self.meta(table);
        let has = |name: &str| meta.and_then(|m| m.field(name)).cloned();
        let mut state = has("state").unwrap_or_else(|| Field::guess("state"));
        if table == "sc_request"
            && let Some(request) = has("request_state")
        {
            state = request;
        }
        let mut states = model::closing_states(&state);
        if states.is_empty()
            && let Some((_, list)) = CLOSING.iter().find(|(t, _)| *t == table)
        {
            states = list
                .iter()
                .map(|(v, l)| ((*v).to_owned(), (*l).to_owned()))
                .collect();
        }
        let Some((value, label)) = states.first().cloned() else {
            self.toast(
                ToastKind::Info,
                format!(
                    "{} has no state that closes a record. Edit its fields instead.",
                    tables::many(table)
                ),
            );
            return None;
        };
        state.kind = Kind::Choice;
        state.choices = states;
        let mut first = FormField::new(state, "", true);
        first.cell = Cell {
            value,
            display: label,
        };
        let mut out = vec![first];
        let needed = matches!(table, "incident" | "change_request" | "problem");
        for name in ["close_code", "resolution_code"] {
            let field = match (has(name), meta) {
                (Some(field), _) => field,
                // The platform requires it on these, known or not.
                (None, None) if name == "close_code" && needed && table != "problem" => {
                    Field::guess(name)
                }
                _ => continue,
            };
            out.push(FormField::new(field, "", needed));
            break;
        }
        for name in ["cause_notes", "fix_notes", "close_notes"] {
            let field = match (has(name), meta) {
                (Some(field), _) => field,
                (None, None) if name == "close_notes" => Field::guess(name),
                _ => continue,
            };
            // A problem has its own notes; the general ones would repeat them.
            if name == "close_notes" && table == "problem" {
                continue;
            }
            let required = needed && name != "cause_notes";
            let mut field = FormField::new(
                Field {
                    kind: Kind::Long,
                    ..field
                },
                if required { "" } else { "Optional" },
                required,
            );
            field.editor.multiline = true;
            out.push(field);
        }
        Some(out)
    }

    /// Checks the form and shows the review.
    pub(super) fn submit_form(&mut self) {
        let Some(Overlay::Form(form)) = self.overlays.last_mut() else {
            return;
        };
        form.error = None;
        if let Some(at) = form
            .fields
            .iter()
            .position(|f| f.required && f.value().value.is_empty())
        {
            form.cursor = at;
            form.error = Some(format!("{} is required", form.fields[at].field.label));
            return;
        }
        let mut body = Map::new();
        let mut lines = Vec::new();
        for f in &form.fields {
            let cell = f.value();
            let adds = f.field.kind == Kind::Journal;
            let name = &f.field.name;
            if cell.value.is_empty()
                || (!adds
                    && form.targets.len() == 1
                    && cell.value == form.targets[0].row.value(name))
            {
                continue;
            }
            body.insert(name.clone(), Value::String(cell.value));
            lines.push(ReviewLine {
                field: name.clone(),
                label: f.field.label.clone(),
                old: match adds {
                    true => String::new(),
                    false => old_text(&form.targets, name),
                },
                new: cell.display,
                adds,
                moved: false,
            });
        }
        let (verb, done) = match form.kind {
            FormKind::Comment => ("Send comment", "comment added"),
            FormKind::WorkNote => ("Save work note", "work note added"),
            FormKind::Assign => ("Assign", "assigned"),
            FormKind::Resolve => ("Close", "closed"),
            FormKind::Approve => ("Approve", "approved"),
            FormKind::Reject => ("Reject", "rejected"),
        };
        if let FormKind::Approve | FormKind::Reject = form.kind {
            let (value, label) = match form.kind {
                FormKind::Approve => ("approved", "Approved"),
                _ => ("rejected", "Rejected"),
            };
            body.insert("state".into(), value.into());
            lines.insert(
                0,
                ReviewLine {
                    field: "state".into(),
                    label: "State".into(),
                    old: old_text(&form.targets, "state"),
                    new: label.into(),
                    adds: false,
                    moved: false,
                },
            );
        }
        if body.is_empty() {
            form.error = Some("Nothing is different yet: choose a person or a group".into());
            return;
        }
        let review = review(
            form.title.clone(),
            (verb, done),
            &form.targets,
            lines,
            &body,
        );
        self.overlays.push(Overlay::Review(review));
    }

    pub(super) fn assign_me(&mut self) {
        if let Some(problem) = self.form_problem(FormKind::Assign, &self.target_rows()) {
            self.toast(ToastKind::Warning, problem);
            return;
        }
        let Some(me) = self.me().cloned() else {
            self.toast(
                ToastKind::Warning,
                "The instance has not said who you are yet. Try again in a moment.",
            );
            return;
        };
        let targets: Vec<Target> = self
            .targets()
            .into_iter()
            .filter(|t| t.row.value("assigned_to") != me.sys_id)
            .collect();
        if targets.is_empty() {
            self.toast(ToastKind::Info, "It is assigned to you already.");
            return;
        }
        let mut body = Map::new();
        body.insert("assigned_to".into(), me.sys_id.clone().into());
        let names = match targets.len() {
            1 => targets[0].name.clone(),
            n => format!("{n} records"),
        };
        let lines = vec![ReviewLine {
            field: "assigned_to".into(),
            label: "Assigned to".into(),
            old: old_text(&targets, "assigned_to"),
            new: me.name.clone(),
            adds: false,
            moved: false,
        }];
        let review = review(
            format!("Assign {names} to me"),
            ("Assign to me", "assigned to you"),
            &targets,
            lines,
            &body,
        );
        self.overlays.push(Overlay::Review(review));
    }

    /// The review of the changes made in the record.
    pub(super) fn review_edits(&mut self) {
        let Some(record) = &self.record else { return };
        if self.connection.read_only {
            self.toast(ToastKind::Warning, self.read_only_text());
            return;
        }
        if record.edits.is_empty() {
            self.toast(
                ToastKind::Info,
                "Nothing is changed. Press Enter on a field to edit it.",
            );
            return;
        }
        if record.is_new()
            && let Some(missing) = self.meta(&record.table).and_then(|m| {
                m.fields
                    .iter()
                    .filter(|f| f.mandatory && f.locked().is_none() && f.kind != Kind::Journal)
                    .find(|f| record.cell(&f.name).value.is_empty())
            })
        {
            let (label, name) = (missing.label.clone(), missing.name.clone());
            let at = self
                .detail_fields(record)
                .iter()
                .filter(|d| matches!(d, model::Detail::Field(_)))
                .position(|d| *d == model::Detail::Field(name.clone()));
            if let (Some(at), Some(record)) = (at, &mut self.record) {
                record.cursor = at;
            }
            self.toast(ToastKind::Warning, format!("{label} is required."));
            return;
        }
        let body: Map<String, Value> = record
            .edits
            .iter()
            .map(|e| (e.field.clone(), Value::String(e.new.value.clone())))
            .collect();
        let lines = record
            .edits
            .iter()
            .map(|e| ReviewLine {
                field: e.field.clone(),
                label: e.label.clone(),
                old: record.row.display(&e.field).to_owned(),
                new: e.new.display.clone(),
                adds: record.is_new(),
                moved: false,
            })
            .collect();
        let review = match record.is_new() {
            true => Review {
                title: record.name(),
                verb: "Create",
                done: "created",
                names: Vec::new(),
                lines,
                work: ReviewWork::Create(record.table.clone(), body),
                state: ReviewState::Ready,
                scroll: 0,
            },
            false => review(
                format!("Change {}", record.name()),
                ("Save", "saved"),
                &[record_target(record)],
                lines,
                &body,
            ),
        };
        self.overlays.push(Overlay::Review(review));
    }

    /// The person confirmed the review: the one place that sends a write.
    pub(super) fn send_review(&mut self) {
        if self.connection.read_only {
            self.toast(ToastKind::Warning, self.read_only_text());
            return;
        }
        let req = self.next_req + 1;
        let Some(Overlay::Review(review)) = self.overlays.last_mut() else {
            return;
        };
        if matches!(review.state, ReviewState::Sending(..)) {
            return;
        }
        review.state = ReviewState::Sending(req, 0);
        let call = match &review.work {
            ReviewWork::Write(writes) => Call::Write {
                req,
                writes: writes.clone(),
            },
            ReviewWork::Create(table, body) => Call::Create {
                req,
                table: table.clone(),
                body: body.clone(),
            },
        };
        self.next_req = req;
        self.call(call);
    }

    fn sending(&self, req: Req) -> Option<usize> {
        self.overlays.iter().position(|o| {
            matches!(o, Overlay::Review(Review { state: ReviewState::Sending(r, _), .. }) if *r == req)
        })
    }

    pub(super) fn written(&mut self, req: Req, results: Vec<Written>) {
        let Some(at) = self.sending(req) else { return };
        let Overlay::Review(mut review) = self.overlays.remove(at) else {
            return;
        };
        let ReviewWork::Write(writes) = &review.work else {
            return;
        };
        let total = writes.len();
        let mut left = Vec::new();
        let mut problems = Vec::new();
        let mut kept_fields: Vec<String> = Vec::new();
        let mut saved = 0;
        let mut conflict = None;
        let mut not_sent = None;
        for (write, result) in writes.clone().into_iter().zip(results) {
            match result {
                Written::Saved { row, kept } => {
                    saved += 1;
                    for field in kept {
                        let label = self.field(&write.table, &field).label.clone();
                        let value = match row.display(&field) {
                            "" => "empty".to_owned(),
                            value => text::cut(&text::line(value), 40, self.theme.g.ellipsis),
                        };
                        kept_fields.push(format!("{label} is still {value}"));
                    }
                    self.take_saved(&write, row);
                }
                Written::Conflict(now) => {
                    let who = match text::line(now.display("sys_updated_by")) {
                        who if who.is_empty() => "Someone".into(),
                        who => who,
                    };
                    let when = text::parse_utc(now.value("sys_updated_on"))
                        .map(|t| text::since(t, self.now));
                    let when = match when.as_deref() {
                        None => "meanwhile",
                        Some("now") => "just now",
                        Some(when) => when,
                    };
                    problems.push(format!("{}: {who} changed it {when}", write.name));
                    if total == 1 {
                        for line in &mut review.lines {
                            if line.adds || !now.has(&line.field) {
                                continue;
                            }
                            let value = now.display(&line.field);
                            line.moved = value != line.old;
                            line.old = value.to_owned();
                        }
                    }
                    let baseline = now.baseline();
                    if let Some(record) = self.record.as_mut().filter(|r| r.sys_id == write.sys_id)
                    {
                        record.row.merge(now);
                        record.baseline = baseline.clone();
                    }
                    conflict = Some((who, when.to_owned()));
                    left.push(Write {
                        baseline: Some(baseline),
                        ..write
                    });
                }
                Written::Failed(fault) if fault.code == "dry_run" => {
                    not_sent = Some(fault.message);
                    left.push(write);
                }
                Written::Failed(fault) => {
                    problems.push(match total {
                        1 => problem_text(&fault, write.baseline.is_none()),
                        _ => format!(
                            "{}: {}",
                            write.name,
                            problem_text(&fault, write.baseline.is_none())
                        ),
                    });
                    left.push(write);
                }
            }
        }
        if left.is_empty() {
            // The form under the review has done its work.
            self.overlays.truncate(at);
            if matches!(self.overlays.last(), Some(Overlay::Form(_))) {
                self.overlays.pop();
            }
            self.list.selected.clear();
            let what = match (total, review.names.first()) {
                (1, _) => writes[0].name.clone(),
                (n, _) => format!("{n} records"),
            };
            if kept_fields.is_empty() {
                self.toast(ToastKind::Good, format!("{what}: {}.", review.done));
            } else {
                kept_fields.dedup();
                self.toast(
                    ToastKind::Warning,
                    format!(
                        "{what}: {}, but the instance kept some values. {}.",
                        review.done,
                        kept_fields.join("; ")
                    ),
                );
            }
            self.after_write();
            return;
        }
        if saved > 0 {
            self.toast(
                ToastKind::Warning,
                format!("{saved} of {total} records: {}.", review.done),
            );
            review.names = left.iter().map(|w| w.name.clone()).collect();
            self.after_write();
        }
        review.state = match conflict {
            Some((who, when)) if total == 1 => ReviewState::Conflict(who, when),
            _ if problems.is_empty() => ReviewState::Ready,
            _ => ReviewState::Failed(problems),
        };
        review.scroll = 0;
        review.work = ReviewWork::Write(left);
        let at = at.min(self.overlays.len());
        self.overlays.insert(at, Overlay::Review(review));
        if let Some(text) = not_sent {
            self.toast(ToastKind::Info, text);
        }
    }

    /// Puts a saved record where it is on screen.
    fn take_saved(&mut self, write: &Write, row: Row) {
        for listed in &mut self.list.rows {
            if listed.sys_id == write.sys_id {
                let mut newer = row.clone();
                // Keep only what the list shows.
                newer.cells.retain(|(n, _)| listed.has(n));
                listed.merge(newer);
            }
        }
        let baseline = row.baseline();
        let records = self
            .record
            .iter_mut()
            .chain(&mut self.records)
            .chain(self.back.iter_mut().flat_map(|s| s.record.iter_mut()));
        for record in records {
            if record.sys_id == write.sys_id {
                record.row.merge(row.clone());
                record.baseline = baseline.clone();
                record.edits.retain(|e| !write.body.contains_key(&e.field));
                // What was written shows up in these.
                record.activity = Load::Idle;
                record.history = Load::Idle;
                record.slas = Load::Idle;
            }
            // An approval changes what the open record's Related tab shows.
            if record.related.iter().any(|(_, l)| {
                l.ready()
                    .is_some_and(|rows| rows.iter().any(|r| r.sys_id == write.sys_id))
            }) {
                record.related.clear();
            }
        }
    }

    fn after_write(&mut self) {
        self.refresh_list();
        self.load_counts();
        self.load_record();
    }

    pub(super) fn created(&mut self, req: Req, outcome: Outcome<Row>) {
        let Some(at) = self.sending(req) else { return };
        match outcome {
            Ok(row) => {
                self.overlays.truncate(at);
                let name = row.name();
                self.toast(ToastKind::Good, format!("{name}: created."));
                self.record = None;
                self.set_record(row, Tab::Details);
                self.focus = Pane::Record;
                self.refresh_list();
                self.load_counts();
            }
            Err(fault) => {
                let dry = fault.code == "dry_run";
                if let Some(Overlay::Review(review)) = self.overlays.get_mut(at) {
                    review.state = match dry {
                        true => ReviewState::Ready,
                        false => ReviewState::Failed(vec![problem_text(&fault, true)]),
                    };
                }
                if dry {
                    self.toast(ToastKind::Info, fault.message);
                }
            }
        }
    }
}

/// A failure, and what it means for the record.
fn problem_text(fault: &Fault, repeats: bool) -> String {
    let mut out = text::line(&fault.message);
    if matches!(fault.code, "network" | "timeout") {
        out.push_str(match repeats {
            // Nothing detects a second note or a second record.
            true => ". It may have been saved anyway: look before you send it again",
            false => ". It may have been saved anyway; sending it again is safe",
        });
    }
    out
}

fn target(row: &Row) -> Target {
    Target {
        table: row.table.clone(),
        sys_id: row.sys_id.clone(),
        name: row.name(),
        row: row.clone(),
        baseline: row.baseline(),
    }
}

fn target_line(target: &Target) -> String {
    let summary = target.row.summary();
    match summary.is_empty() {
        true => target.name.clone(),
        false => format!("{}  {summary}", target.name),
    }
}

fn record_target(record: &RecordState) -> Target {
    Target {
        table: record.table.clone(),
        sys_id: record.sys_id.clone(),
        name: record.name(),
        row: record.row.clone(),
        baseline: record.baseline.clone(),
    }
}

/// The review of one change to every target. What only adds overwrites
/// nothing, so it checks no version.
fn review(
    title: String,
    (verb, done): (&'static str, &'static str),
    targets: &[Target],
    lines: Vec<ReviewLine>,
    body: &Map<String, Value>,
) -> Review {
    let only_adds = lines.iter().all(|l| l.adds);
    let writes = targets
        .iter()
        .map(|t| Write {
            table: t.table.clone(),
            sys_id: t.sys_id.clone(),
            name: t.name.clone(),
            body: body.clone(),
            baseline: (!only_adds).then(|| t.baseline.clone()),
        })
        .collect();
    Review {
        title,
        verb,
        done,
        names: targets.iter().map(target_line).collect(),
        lines,
        work: ReviewWork::Write(writes),
        state: ReviewState::Ready,
        scroll: 0,
    }
}

/// The cell that all targets share.
fn same_cell<'a>(targets: &'a [Target], field: &str) -> Option<&'a Cell> {
    let cell = targets.first()?.row.cell(field);
    targets
        .iter()
        .all(|t| t.row.cell(field) == cell)
        .then_some(cell)
}

/// What a review shows as the old value. A row of a list can lack the field,
/// which says nothing about its value.
fn old_text(targets: &[Target], field: &str) -> String {
    let known = targets.len() == 1 || targets.iter().all(|t| t.row.has(field));
    same_cell(targets, field)
        .filter(|_| known)
        .map_or_else(|| "(various)".into(), |c| c.display.clone())
}
