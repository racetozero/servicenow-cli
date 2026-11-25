//! Change requests (Change Management API).

use super::args::{BodyArgs, DisplayArg, PageArgs, record_of, show};
use super::table::written;
use crate::ctx::Ctx;
use crate::output::{Col, col, columns, flex};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Map, Value, json};
use sn_core::client::{Method, Request};
use sn_core::{Client, Error, Result, query};

const BASE: &str = "/api/sn_chg_rest/change";
const DEFAULT_LIMIT: u64 = 100;

#[derive(Subcommand, Debug)]
pub enum ChangeCommand {
    /// List change requests.
    #[command(visible_alias = "ls")]
    List(ListChanges),
    /// Read a change request.
    #[command(visible_alias = "show")]
    Get(GetChange),
    /// Create a change request.
    Create(CreateChange),
    /// Change fields of a change request.
    Update(UpdateChange),
    /// Delete a change request.
    #[command(visible_alias = "rm")]
    Delete {
        /// Number (CHG0000001), sys_id, or link.
        change: String,
        /// Use the endpoint of this type.
        #[arg(long, value_enum)]
        r#type: Option<ChangeType>,
    },
    /// States that the change can move to.
    Nextstates {
        /// Number, sys_id, or link.
        change: String,
    },
    /// Approve or reject: --field state=approved.
    Approvals {
        /// Number, sys_id, or link.
        change: String,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Calculate the risk of the change.
    Risk {
        /// Number, sys_id, or link.
        change: String,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Time windows in which the change can be done.
    Schedule {
        /// Number, sys_id, or link.
        change: String,
    },
    /// Tasks of a change.
    #[command(subcommand)]
    Task(TaskCommand),
    /// Configuration items of a change.
    #[command(subcommand)]
    Ci(CiCommand),
    /// Conflicts of a change.
    #[command(subcommand)]
    Conflict(ConflictCommand),
    /// Change models, or one of them.
    Models {
        /// sys_id of a model.
        sys_id: Option<String>,
    },
    /// Standard change templates, or one of them.
    Templates {
        /// sys_id of a template.
        sys_id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ChangeType {
    Normal,
    Emergency,
    Standard,
}

#[derive(Args, Debug)]
pub struct ShapeArgs {
    /// Fields, comma-separated.
    #[arg(long, short = 'f', value_delimiter = ',', alias = "sysparm-fields")]
    fields: Vec<String>,
    #[command(flatten)]
    display: DisplayArg,
    /// Leave the links of reference fields out.
    #[arg(long, alias = "sysparm-exclude-reference-link")]
    exclude_reference_link: bool,
    /// Use the fields of this form or list view.
    #[arg(long, value_name = "VIEW", alias = "sysparm-view")]
    view: Option<String>,
}

impl ShapeArgs {
    /// Without flags the API answers with labels and values.
    fn apply(&self, request: Request) -> Request {
        let fields = (!self.fields.is_empty()).then(|| self.fields.join(","));
        let display = self
            .display
            .display_value
            .map(|_| self.display.value().as_str());
        request
            .query_opt("sysparm_fields", fields)
            .query_opt("sysparm_display_value", display)
            .query_opt(
                "sysparm_exclude_reference_link",
                self.exclude_reference_link.then_some("true"),
            )
            .query_opt("sysparm_view", self.view.clone())
    }
}

#[derive(Args, Debug)]
pub struct ListChanges {
    /// Only this type.
    #[arg(long, value_enum)]
    r#type: Option<ChangeType>,
    /// Encoded query. This API cannot sort.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    query: Option<String>,
    #[command(flatten)]
    page: PageArgs,
    #[command(flatten)]
    shape: ShapeArgs,
}

#[derive(Args, Debug)]
pub struct GetChange {
    /// Number (CHG0000001), sys_id, or link.
    change: String,
    /// Use the endpoint of this type.
    #[arg(long, value_enum)]
    r#type: Option<ChangeType>,
    #[command(flatten)]
    shape: ShapeArgs,
}

#[derive(Args, Debug)]
pub struct CreateChange {
    /// Type of the change.
    #[arg(long, value_enum, default_value_t = ChangeType::Normal)]
    r#type: ChangeType,
    /// sys_id of the standard change template, for --type standard.
    #[arg(long, value_name = "SYS_ID")]
    template: Option<String>,
    #[command(flatten)]
    body: BodyArgs,
    #[command(flatten)]
    shape: ShapeArgs,
}

#[derive(Args, Debug)]
pub struct UpdateChange {
    /// Number (CHG0000001), sys_id, or link.
    change: String,
    /// Use the endpoint of this type.
    #[arg(long, value_enum)]
    r#type: Option<ChangeType>,
    #[command(flatten)]
    body: BodyArgs,
    #[command(flatten)]
    shape: ShapeArgs,
}

#[derive(Subcommand, Debug)]
pub enum TaskCommand {
    /// List the tasks.
    List {
        /// Number, sys_id, or link of the change.
        change: String,
        /// Fields, comma-separated.
        #[arg(long, short = 'f', value_delimiter = ',', alias = "sysparm-fields")]
        fields: Vec<String>,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Read a task.
    Get {
        /// Number, sys_id, or link of the change.
        change: String,
        /// Number (CTASK0000001) or sys_id of the task.
        task: String,
    },
    /// Create a task.
    Create {
        /// Number, sys_id, or link of the change.
        change: String,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Change fields of a task.
    Update {
        /// Number, sys_id, or link of the change.
        change: String,
        /// Number (CTASK0000001) or sys_id of the task.
        task: String,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Delete a task.
    Delete {
        /// Number, sys_id, or link of the change.
        change: String,
        /// Number (CTASK0000001) or sys_id of the task.
        task: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum CiCommand {
    /// List the configuration items.
    List {
        /// Number, sys_id, or link of the change.
        change: String,
    },
    /// Add configuration items: --data '{"cmdb_ci_sys_ids":["..."],"association_type":"affected"}'.
    Add {
        /// Number, sys_id, or link of the change.
        change: String,
        #[command(flatten)]
        body: BodyArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConflictCommand {
    /// Read the conflicts and the state of the check.
    Get {
        /// Number, sys_id, or link of the change.
        change: String,
    },
    /// Start a conflict check.
    Add {
        /// Number, sys_id, or link of the change.
        change: String,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Stop the check and remove all conflicts.
    Remove {
        /// Number, sys_id, or link of the change.
        change: String,
    },
}

fn base(kind: Option<ChangeType>) -> String {
    match kind {
        None => BASE.to_owned(),
        Some(ChangeType::Normal) => format!("{BASE}/normal"),
        Some(ChangeType::Emergency) => format!("{BASE}/emergency"),
        Some(ChangeType::Standard) => format!("{BASE}/standard"),
    }
}

async fn change_id(client: &Client, change: &str) -> Result<String> {
    record_of(client, "change_request", change).await
}

pub async fn run(ctx: &Ctx, command: ChangeCommand) -> Result<()> {
    match command {
        ChangeCommand::List(args) => list(ctx, args).await,
        ChangeCommand::Get(args) => {
            let client = ctx.client().await?;
            let id = change_id(client, &args.change).await?;
            let request = Request::new(Method::GET, format!("{}/{id}", base(args.r#type)));
            let found = client.send(args.shape.apply(request)).await?.result()?;
            show(ctx, found)
        }
        ChangeCommand::Create(args) => create(ctx, args).await,
        ChangeCommand::Update(args) => {
            let body = args.body.required()?;
            let client = ctx.client().await?;
            let id = change_id(client, &args.change).await?;
            let request = Request::new(Method::PATCH, format!("{}/{id}", base(args.r#type)))
                .json(body.into());
            let changed = client.send(args.shape.apply(request)).await?.result()?;
            written(ctx, "Updated", "change_request", changed)
        }
        ChangeCommand::Delete { change, r#type } => {
            let client = ctx.client().await?;
            let id = change_id(client, &change).await?;
            ctx.confirm(&format!("delete the change {change}"))?;
            client.delete(format!("{}/{id}", base(r#type))).await?;
            ctx.out.done(
                json!({ "table": "change_request", "sys_id": id, "deleted": true }),
                &format!("Deleted change {change}"),
            )
        }
        ChangeCommand::Nextstates { change } => {
            read(ctx, &change, "nextstates", |ctx, found| {
                ctx.out.item_with(found, json!({}), next_states)
            })
            .await
        }
        ChangeCommand::Approvals { change, body } => {
            write(ctx, Method::PATCH, &change, "approvals", body.required()?).await
        }
        ChangeCommand::Risk { change, body } => {
            write(ctx, Method::PATCH, &change, "risk", body.required()?).await
        }
        ChangeCommand::Schedule { change } => read(ctx, &change, "schedule", show).await,
        ChangeCommand::Task(command) => task(ctx, command).await,
        ChangeCommand::Ci(CiCommand::List { change }) => read(ctx, &change, "ci", show).await,
        ChangeCommand::Ci(CiCommand::Add { change, body }) => {
            write(ctx, Method::POST, &change, "ci", body.required()?).await
        }
        ChangeCommand::Conflict(ConflictCommand::Get { change }) => {
            read(ctx, &change, "conflict", show).await
        }
        ChangeCommand::Conflict(ConflictCommand::Add { change, body }) => {
            write(ctx, Method::POST, &change, "conflict", body.object()?).await
        }
        ChangeCommand::Conflict(ConflictCommand::Remove { change }) => {
            let client = ctx.client().await?;
            let id = change_id(client, &change).await?;
            ctx.confirm(&format!("remove all conflicts of the change {change}"))?;
            client.delete(format!("{BASE}/{id}/conflict")).await?;
            ctx.out.done(
                json!({ "change": id, "conflicts_removed": true }),
                &format!("Removed the conflicts of {change}"),
            )
        }
        ChangeCommand::Models { sys_id } => reference(ctx, "model", sys_id).await,
        ChangeCommand::Templates { sys_id } => reference(ctx, "standard/template", sys_id).await,
    }
}

async fn read(
    ctx: &Ctx,
    change: &str,
    part: &str,
    print: impl FnOnce(&Ctx, Value) -> Result<()>,
) -> Result<()> {
    let client = ctx.client().await?;
    let id = change_id(client, change).await?;
    let found = client.get(format!("{BASE}/{id}/{part}")).await?;
    match ctx.out.raw {
        true => show(ctx, found),
        false => print(ctx, found),
    }
}

async fn write(
    ctx: &Ctx,
    method: Method,
    change: &str,
    part: &str,
    body: Map<String, Value>,
) -> Result<()> {
    let client = ctx.client().await?;
    let id = change_id(client, change).await?;
    let request = Request::new(method, format!("{BASE}/{id}/{part}")).json(body.into());
    show(ctx, client.send(request).await?.result()?)
}

async fn list(ctx: &Ctx, args: ListChanges) -> Result<()> {
    let query = args.query.unwrap_or_default();
    // The API drops sort terms, and an arbitrary order is a wrong answer.
    if let Some(sort) = query::terms(&query).find(|term| term.starts_with("ORDERBY")) {
        return Err(Error::invalid(format!(
            "you passed {sort}, but the Change API cannot sort; run `sn table list change_request -q '{query}'`, which can"
        )));
    }
    let request = Request::new(Method::GET, base(args.r#type))
        .query_opt("sysparm_query", (!query.is_empty()).then_some(query));
    let request = args.shape.apply(args.page.apply(request, DEFAULT_LIMIT));
    let answer = ctx.client().await?.send(request).await?.result()?;
    let cols = match args.shape.fields.is_empty() {
        false => columns(&args.shape.fields),
        true => vec![
            col("NUMBER", "number"),
            col("TYPE", "type"),
            col("STATE", "state"),
            col("RISK", "risk"),
            col("START", "start_date"),
            col("GROUP", "assignment_group"),
            flex("SHORT DESCRIPTION", "short_description"),
        ],
    };
    emit(ctx, answer, &cols, |count| {
        args.page.meta(count, None, DEFAULT_LIMIT)
    })
}

async fn create(ctx: &Ctx, args: CreateChange) -> Result<()> {
    let body = args.body.object()?;
    let path = match (args.r#type, &args.template) {
        (ChangeType::Standard, None) => {
            return Err(Error::Input {
                arg: "template".into(),
                message: "a standard change needs its template; pass --template SYS_ID (list them with `sn change templates`)".into(),
                choices: Vec::new(),
            });
        }
        (ChangeType::Standard, Some(template)) => {
            query::check_name("template", template)?;
            format!("{}/{template}", base(Some(ChangeType::Standard)))
        }
        (kind, None) => base(Some(kind)),
        (_, Some(_)) => {
            return Err(Error::invalid(
                "you passed --template, which is for standard changes; add --type standard, or leave --template out",
            ));
        }
    };
    let request = args
        .shape
        .apply(Request::new(Method::POST, path).json(body.into()));
    let made = ctx.client().await?.send(request).await?.result()?;
    written(ctx, "Created", "change_request", made)
}

async fn task(ctx: &Ctx, command: TaskCommand) -> Result<()> {
    let client = ctx.client().await?;
    match command {
        TaskCommand::List {
            change,
            fields,
            page,
        } => {
            let id = change_id(client, &change).await?;
            let request = Request::new(Method::GET, format!("{BASE}/{id}/task")).query_opt(
                "sysparm_fields",
                (!fields.is_empty()).then(|| fields.join(",")),
            );
            let answer = client
                .send(page.apply(request, DEFAULT_LIMIT))
                .await?
                .result()?;
            let cols = match fields.is_empty() {
                false => columns(&fields),
                true => vec![
                    col("NUMBER", "number"),
                    col("TYPE", "change_task_type"),
                    col("STATE", "state"),
                    col("ASSIGNED TO", "assigned_to"),
                    flex("SHORT DESCRIPTION", "short_description"),
                ],
            };
            emit(ctx, answer, &cols, |count| {
                page.meta(count, None, DEFAULT_LIMIT)
            })
        }
        TaskCommand::Create { change, body } => {
            let body = body.object()?;
            let id = change_id(client, &change).await?;
            let made = client
                .post(format!("{BASE}/{id}/task"), body.into())
                .await?;
            written(ctx, "Created", "change_task", made)
        }
        TaskCommand::Get { change, task } => {
            let path = task_path(client, &change, &task).await?;
            show(ctx, client.get(path).await?)
        }
        TaskCommand::Update { change, task, body } => {
            let body = body.required()?;
            let path = task_path(client, &change, &task).await?;
            let changed = client.patch(path, body.into()).await?;
            written(ctx, "Updated", "change_task", changed)
        }
        TaskCommand::Delete { change, task } => {
            let path = task_path(client, &change, &task).await?;
            ctx.confirm(&format!("delete the task {task} of the change {change}"))?;
            client.delete(path).await?;
            ctx.out.done(
                json!({ "table": "change_task", "change": change, "task": task, "deleted": true }),
                &format!("Deleted task {task}"),
            )
        }
    }
}

async fn task_path(client: &Client, change: &str, task: &str) -> Result<String> {
    let (change, task) = tokio::try_join!(
        change_id(client, change),
        record_of(client, "change_task", task)
    )?;
    Ok(format!("{BASE}/{change}/task/{task}"))
}

/// Models and templates.
async fn reference(ctx: &Ctx, path: &str, sys_id: Option<String>) -> Result<()> {
    let client = ctx.client().await?;
    let Some(id) = sys_id else {
        let answer = client.get(format!("{BASE}/{path}")).await?;
        let cols = [
            col("SYS_ID", "sys_id"),
            col("NAME", "name"),
            col("ACTIVE", "active"),
            flex("SHORT DESCRIPTION", "short_description"),
        ];
        return emit(ctx, answer, &cols, |_| json!({}));
    };
    query::check_name("sys_id", &id)?;
    let answer = client.get(format!("{BASE}/{path}/{id}")).await?;
    show(ctx, answer)
}

/// Prints a list. The API ends most lists with an element that is not a
/// record but says which query fields it ignored.
fn emit(ctx: &Ctx, answer: Value, cols: &[Col], meta: impl FnOnce(usize) -> Value) -> Result<()> {
    let Value::Array(mut rows) = answer else {
        return show(ctx, answer);
    };
    let about = rows
        .last()
        .and_then(Value::as_object)
        .filter(|last| last.len() == 1)
        .and_then(|last| last.get("__meta"));
    let ignored: Vec<String> = about
        .and_then(|about| about["fields"]["ignored"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        // Its own parameters are reported too; they did work.
        .filter(|name| !name.is_empty() && !name.starts_with("sysparm_"))
        .map(str::to_owned)
        .collect();
    let has_about = about.is_some();
    if !ignored.is_empty() {
        ctx.out.warn(&format!(
            "the instance ignored the query on {}, so the list is not filtered by it; check the field names",
            ignored.join(", ")
        ));
    }
    if ctx.out.raw {
        return show(ctx, rows.into());
    }
    if has_about {
        rows.pop();
    }
    let mut meta = meta(rows.len());
    if !ignored.is_empty() {
        meta["ignored_query_fields"] = ignored.into();
    }
    ctx.out.list(rows, cols, meta)
}

/// The states by label.
fn next_states(found: &Value) -> String {
    let labels = &found["state_label"];
    let states: Vec<String> = found["available_states"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|state| {
            let value = crate::output::cell_text(state);
            match labels[value.as_str()].as_str() {
                Some(label) => format!("{label} ({value})"),
                None => value,
            }
        })
        .collect();
    match states.is_empty() {
        true => crate::output::fields_text(found),
        false => states.join("\n"),
    }
}
