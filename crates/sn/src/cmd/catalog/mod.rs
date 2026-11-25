//! Service Catalog: the API's own calls, and an order workflow that checks
//! everything before it orders.

mod item;
mod order;

use super::args::{BodyArgs, DisplayArg, PageArgs, dropped, show, text_or_file};
use crate::ctx::Ctx;
use crate::output::{bold, cell, col, fields_text, flex};
use clap::{Args, Subcommand};
use item::{Inputs, Item, REDACTED, UNORDERABLE, is_orderable_class, kind_label};
use order::{Scrub, Unverified};
use serde_json::{Map, Value, json};
use sn_core::client::{Method, Request};
use sn_core::refs::caller;
use sn_core::refs::{self, Person};
use sn_core::table::{raw, text};
use sn_core::{Choice, Client, DisplayValue, Error, Result, TableQuery, is_sys_id, query};

const BASE: &str = "/api/sn_sc/servicecatalog";
const DEFAULT_LIMIT: u64 = 100;
const SEARCH_LIMIT: u64 = 20;

#[derive(Subcommand, Debug)]
pub enum CatalogCommand {
    /// Find items, record producers and order guides.
    Search {
        /// Text to search for; everything without it.
        text: Option<String>,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Show an item and the variables that an order takes.
    Show {
        /// sys_id, name, or a text that finds one item.
        item: String,
    },
    /// Order an item after checking every variable.
    Order(Box<OrderArgs>),
    /// Show your cart (the one of the portal), or order, change or empty it.
    Cart {
        #[command(subcommand)]
        command: Option<CartCommand>,
    },
    /// Items that you requested, and their variables.
    #[command(visible_alias = "request")]
    Requests {
        #[command(subcommand)]
        command: RequestsCommand,
    },
    /// List catalogs.
    List {
        /// Only catalogs that contain this text.
        #[arg(long, alias = "sysparm-text")]
        text: Option<String>,
    },
    /// Read a catalog.
    Get {
        /// sys_id of the catalog.
        sys_id: String,
    },
    /// List the categories of a catalog.
    Categories {
        /// sys_id of the catalog.
        catalog: String,
        #[command(flatten)]
        page: PageArgs,
        /// Only top-level categories.
        #[arg(long, alias = "sysparm-top-level-only")]
        top_level_only: bool,
    },
    /// Read a category.
    Category {
        /// sys_id of the category.
        sys_id: String,
    },
    /// List items as the API returns them.
    Items(Box<ItemsArgs>),
    /// Read an item as the API returns it.
    Item {
        /// sys_id of the item.
        sys_id: String,
    },
    /// Read the variables of an item as the API returns them.
    ItemVariables {
        /// sys_id of the item.
        sys_id: String,
    },
    /// Add an item to the cart: --data '{"sysparm_quantity":"1","variables":{...}}'.
    AddToCart {
        /// sys_id of the item.
        sys_id: String,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Change an item in the cart.
    CartUpdate {
        /// sys_id of the cart item.
        cart_item_id: String,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Remove an item from the cart (the same as `cart remove`).
    CartRemove {
        /// sys_id of the cart item.
        cart_item_id: String,
    },
    /// Empty a cart.
    CartEmpty {
        /// sys_id of the cart.
        sys_id: String,
    },
    /// Check out the cart; with one-step checkout this orders it.
    Checkout,
    /// Order the cart as it is (`cart submit` checks it first).
    SubmitOrder,
    /// List your wish list.
    Wishlist,
}

#[derive(Args, Debug)]
pub struct ItemsArgs {
    /// Only items that contain this text.
    #[arg(long, alias = "sysparm-text")]
    text: Option<String>,
    /// sys_id of a category.
    #[arg(long, alias = "sysparm-category")]
    category: Option<String>,
    /// sys_id of a catalog.
    #[arg(long, alias = "sysparm-catalog")]
    catalog: Option<String>,
    /// Type, for example record_producer.
    #[arg(long, alias = "sysparm-type")]
    item_type: Option<String>,
    #[command(flatten)]
    page: PageArgs,
}

#[derive(Args, Debug)]
pub struct OrderArgs {
    /// sys_id or exact name of the item.
    item: String,
    /// Variable name=value (repeatable); it wins over --vars-file.
    #[arg(long = "var", value_name = "NAME=VALUE", allow_hyphen_values = true)]
    vars: Vec<String>,
    /// YAML or JSON file of variable values, or - for stdin (then add --yes).
    #[arg(long, value_name = "FILE")]
    vars_file: Option<String>,
    /// How many to order.
    #[arg(long, default_value_t = 1)]
    quantity: u32,
    /// Order for this user: user name, e-mail address, full name, sys_id.
    #[arg(long, value_name = "USER")]
    requested_for: Option<String>,
    /// Warn about variables that the item does not have, and leave them out.
    #[arg(long)]
    allow_unknown_vars: bool,
    /// Order although your date format cannot be read.
    #[arg(long)]
    allow_unverified_formats: bool,
    /// Order through the cart, which must be empty. For instances that answer an order with a cart.
    #[arg(long)]
    checkout: bool,
    #[command(flatten)]
    body: BodyArgs,
}

#[derive(Subcommand, Debug)]
pub enum CartCommand {
    /// Show the cart. The instance makes an empty one when you have none.
    Show,
    /// Order everything in the cart.
    Submit,
    /// Remove one item.
    #[command(visible_alias = "rm")]
    Remove {
        /// sys_id of the cart item, from `catalog cart`.
        cart_item_id: String,
    },
    /// Remove all items.
    #[command(visible_alias = "empty")]
    Clear,
}

#[derive(Subcommand, Debug)]
pub enum RequestsCommand {
    /// List the items that you requested or opened, newest first.
    Mine {
        #[command(flatten)]
        page: PageArgs,
        /// Only open ones.
        #[arg(long)]
        open: bool,
        #[command(flatten)]
        display: DisplayArg,
    },
    /// Print the variables of a requested item, in the form that `order --vars-file` takes.
    Vars {
        /// Number (RITM0010001) or sys_id.
        item: String,
        /// Variables without a value, too.
        #[arg(long)]
        include_empty: bool,
    },
}

pub async fn run(ctx: &Ctx, command: CatalogCommand) -> Result<()> {
    let client = ctx.client().await?;
    let get = async |path: String| show(ctx, client.get(format!("{BASE}{path}")).await?);
    match command {
        CatalogCommand::Search { text, page } => search(ctx, text, page).await,
        CatalogCommand::Show { item } => show_item(ctx, &item).await,
        CatalogCommand::Order(args) => place(ctx, *args).await,
        CatalogCommand::Cart { command } => cart(ctx, command.unwrap_or(CartCommand::Show)).await,
        CatalogCommand::Requests { command } => match command {
            RequestsCommand::Mine {
                page,
                open,
                display,
            } => requests(ctx, page, open, display).await,
            RequestsCommand::Vars {
                item,
                include_empty,
            } => request_variables(ctx, &item, include_empty).await,
        },
        CatalogCommand::List { text } => {
            let request = Request::new(Method::GET, format!("{BASE}/catalogs"))
                .query_opt("sysparm_text", text);
            show(ctx, client.send(request).await?.result()?)
        }
        CatalogCommand::Get { sys_id } => get(format!("/catalogs/{}", id(&sys_id)?)).await,
        CatalogCommand::Categories {
            catalog,
            page,
            top_level_only,
        } => {
            let path = format!("{BASE}/catalogs/{}/categories", id(&catalog)?);
            let request = Request::new(Method::GET, path)
                .query_opt("sysparm_top_level_only", top_level_only.then_some("true"));
            let found = client
                .send(page.apply(request, DEFAULT_LIMIT))
                .await?
                .result()?;
            listed(ctx, found, &page)
        }
        CatalogCommand::Category { sys_id } => get(format!("/categories/{}", id(&sys_id)?)).await,
        CatalogCommand::Items(args) => {
            let request = Request::new(Method::GET, format!("{BASE}/items"))
                .query_opt("sysparm_text", args.text)
                .query_opt("sysparm_category", args.category)
                .query_opt("sysparm_catalog", args.catalog)
                .query_opt("sysparm_type", args.item_type);
            let found = client
                .send(args.page.apply(request, DEFAULT_LIMIT))
                .await?
                .result()?;
            listed(ctx, found, &args.page)
        }
        CatalogCommand::Item { sys_id } => get(format!("/items/{}", id(&sys_id)?)).await,
        CatalogCommand::ItemVariables { sys_id } => {
            get(format!("/items/{}/variables", id(&sys_id)?)).await
        }
        CatalogCommand::AddToCart { sys_id, body } => {
            let path = format!("{BASE}/items/{}/add_to_cart", id(&sys_id)?);
            let request = Request::new(Method::POST, path).json(body.object()?.into());
            let added = order::send_once(
                client,
                request,
                &Scrub::No,
                "add-to-cart",
                order::MAY_BE_IN_CART,
            )
            .await?;
            ctx.out.done(added, "Added to your cart")
        }
        CatalogCommand::CartUpdate { cart_item_id, body } => {
            let path = format!("{BASE}/cart/{}", id(&cart_item_id)?);
            let changed = client.put(path, body.required()?.into()).await?;
            ctx.out.done(changed, "Changed the item in your cart")
        }
        CatalogCommand::CartRemove { cart_item_id } => {
            cart(ctx, CartCommand::Remove { cart_item_id }).await
        }
        CatalogCommand::CartEmpty { sys_id } => {
            let path = format!("{BASE}/cart/{}/empty", id(&sys_id)?);
            ctx.confirm(&format!("remove all items from the cart {sys_id}"))?;
            client.delete(path).await?;
            ctx.out.done(
                json!({ "cart_id": sys_id, "cleared": true }),
                "Emptied the cart",
            )
        }
        CatalogCommand::Checkout => {
            ctx.confirm(
                "check out your cart, which orders it when the instance has one-step checkout",
            )?;
            let request =
                Request::new(Method::POST, format!("{BASE}/cart/checkout")).json(json!({}));
            let answer = order::send_once(
                client,
                request,
                &Scrub::No,
                "checkout",
                order::MAY_HAVE_ORDERED,
            )
            .await?;
            show(ctx, answer)
        }
        CatalogCommand::SubmitOrder => {
            ctx.confirm("order everything in your cart")?;
            let answer = order::submit_cart(client, &Scrub::No).await?;
            order::placed(client, &answer, true)?;
            show(ctx, answer)
        }
        CatalogCommand::Wishlist => get("/wishlist".to_owned()).await,
    }
}

fn id(value: &str) -> Result<&str> {
    query::check_name("sys_id", value).map(|()| value)
}

fn listed(ctx: &Ctx, found: Value, page: &PageArgs) -> Result<()> {
    match found {
        Value::Array(rows) if !ctx.out.raw => {
            let meta = page.meta(rows.len(), None, DEFAULT_LIMIT);
            ctx.out.list(rows, &[], meta)
        }
        other => show(ctx, other),
    }
}

async fn search_page(
    client: &Client,
    text: Option<&str>,
    page: &PageArgs,
    default_limit: u64,
) -> Result<Vec<Value>> {
    let text = text.map(str::trim).filter(|t| !t.is_empty());
    let request =
        Request::new(Method::GET, format!("{BASE}/items")).query_opt("sysparm_text", text);
    match client
        .send(page.apply(request, default_limit))
        .await?
        .result()?
    {
        Value::Array(rows) => Ok(rows),
        _ => Ok(Vec::new()),
    }
}

async fn search(ctx: &Ctx, text: Option<String>, page: PageArgs) -> Result<()> {
    let client = ctx.client().await?;
    let found = search_page(client, text.as_deref(), &page, SEARCH_LIMIT).await?;
    let meta = page.meta(found.len(), None, SEARCH_LIMIT);
    let rows = found
        .iter()
        .map(|item| {
            let class = item["sys_class_name"].as_str().unwrap_or_default();
            json!({
                "sys_id": item["sys_id"],
                "name": item["name"],
                "short_description": item["short_description"],
                "class": class,
                "kind": kind_label(class),
                "orderable": is_orderable_class(class),
                "category": item["category"]["title"],
            })
        })
        .collect();
    let cols = [
        col("NAME", "name"),
        col("KIND", "kind"),
        flex("SHORT DESCRIPTION", "short_description"),
        col("SYS_ID", "sys_id"),
    ];
    ctx.out.list(rows, &cols, meta)
}

async fn read_item(client: &Client, sys_id: &str) -> Result<Item> {
    match client.get(format!("{BASE}/items/{sys_id}")).await {
        Ok(item) => Item::parse(&item),
        // The API answers an unknown item with a plain 400.
        Err(Error::Api(failure)) if matches!(failure.status, Some(400 | 404)) => {
            Err(Error::NotFound(format!(
                "there is no catalog item {sys_id} that you can order ({}); find items with `sn catalog search`",
                failure.message
            )))
        }
        Err(err) => Err(err),
    }
}

/// An item by sys_id or exact name. With `fuzzy`, a search that finds one
/// item counts too; an order never takes a guess.
async fn find_item(client: &Client, given: &str, fuzzy: bool) -> Result<Item> {
    const PAGE: u64 = 100;
    const MAX: usize = 1000;
    let given = given.trim();
    if is_sys_id(given) {
        return read_item(client, &given.to_ascii_lowercase()).await;
    }
    // A second item of the same name can be on a later page.
    let mut hits: Vec<Value> = Vec::new();
    loop {
        let page = PageArgs {
            limit: Some(PAGE),
            offset: Some(hits.len() as u64),
        };
        let found = search_page(client, Some(given), &page, PAGE).await?;
        let full = found.len() as u64 == PAGE;
        hits.extend(found);
        if !full {
            break;
        }
        if hits.len() >= MAX {
            return Err(Error::invalid(format!(
                "you passed \"{given}\", which matches more than {MAX} items, so the name cannot be checked; pass the sys_id"
            )));
        }
    }
    let exact: Vec<&Value> = hits
        .iter()
        .filter(|hit| text(hit, "name").eq_ignore_ascii_case(given))
        .collect();
    let candidates = |items: &[&Value]| -> Vec<Choice> {
        items
            .iter()
            .take(10)
            .map(|item| {
                let label = format!(
                    "{} ({})",
                    text(item, "name"),
                    kind_label(text(item, "sys_class_name"))
                );
                Choice::new(text(item, "sys_id"), label)
            })
            .collect()
    };
    let chosen = match (exact.as_slice(), hits.as_slice()) {
        ([item], _) => *item,
        ([], [only]) if fuzzy => only,
        ([], []) => {
            return Err(Error::NotFound(format!(
                "no catalog item matches \"{given}\"; search with `sn catalog search TEXT`"
            )));
        }
        ([], _) => {
            return Err(Error::Input {
                arg: "item".into(),
                message: format!(
                    "no catalog item is named \"{given}\"; pass the exact name or the sys_id of one that the search found"
                ),
                choices: candidates(&hits.iter().collect::<Vec<_>>()),
            });
        }
        (several, _) => {
            return Err(Error::Input {
                arg: "item".into(),
                message: format!(
                    "{} catalog items are named \"{given}\"; pass the sys_id of one",
                    several.len()
                ),
                choices: candidates(several),
            });
        }
    };
    read_item(client, text(chosen, "sys_id")).await
}

async fn show_item(ctx: &Ctx, given: &str) -> Result<()> {
    let client = ctx.client().await?;
    let item = find_item(client, given, true).await?;
    let base = client.base();
    ctx.out.item_with(item.to_json(base), json!({}), |_| {
        let mut out = format!(
            "{}\n{}\n{} ({}) {}\n{}\n",
            bold(&item.name, ctx.out.tty),
            item.short_description,
            kind_label(&item.class),
            item.class,
            item.sys_id,
            item.portal_url(base)
        );
        let mut notes = Vec::new();
        if let Err(why) = item.check_orderable(base) {
            notes.push(why.to_string());
        }
        if item.instance_orderable == Some(false) {
            notes.push(UNORDERABLE.to_owned());
        }
        if item.client_logic {
            notes.push("the item has client logic, which only the portal runs".into());
        }
        if !item.widgets.is_empty() {
            notes.push(format!(
                "the item has portal widgets: {}",
                item.widgets.join(", ")
            ));
        }
        for note in notes {
            out.push_str(&format!("Note: {note}\n"));
        }
        if item.variables.is_empty() {
            out.push_str("\nThis item has no variables.");
            return out;
        }
        let rows: Vec<Value> = item
            .variables
            .iter()
            .map(|v| {
                let values: Vec<&str> = v.choices.iter().map(|(value, _)| value.as_str()).collect();
                let options = match (&v.table, values.is_empty()) {
                    (_, false) => values.join(", "),
                    (Some(table), true) => format!("-> {table}"),
                    _ => String::new(),
                };
                json!({
                    "name": v.name,
                    "label": v.label,
                    "type": v.type_name,
                    "required": if v.mandatory { "yes" } else { "" },
                    "default": v.shown_default(),
                    "options": options,
                })
            })
            .collect();
        let cols = [
            col("NAME", "name"),
            col("LABEL", "label"),
            col("TYPE", "type"),
            col("REQUIRED", "required"),
            col("DEFAULT", "default"),
            flex("OPTIONS", "options"),
        ];
        let width = crate::output::terminal_size().map_or(160, |(width, _)| width);
        let lines = crate::output::table_lines(&rows, &cols, width, false, ctx.out.no_truncate);
        out.push('\n');
        out.push_str(&lines.join("\n"));
        out
    })
}

async fn place(ctx: &Ctx, args: OrderArgs) -> Result<()> {
    if args.checkout && args.requested_for.is_some() {
        return Err(Error::invalid(
            "you passed --checkout with --requested-for, but the instance ignores who an item in a cart is for, so the request would be yours; leave --checkout out",
        ));
    }
    let checked = !args.vars.is_empty() || args.vars_file.is_some();
    if !args.body.is_empty() {
        let workflow = checked
            || args.checkout
            || args.quantity != 1
            || args.requested_for.is_some()
            || args.allow_unknown_vars
            || args.allow_unverified_formats;
        if workflow {
            return Err(Error::invalid(
                "you passed --data or --field, which send the body as it is, together with flags of the checked order; use --var and --vars-file, or put everything in --data",
            ));
        }
        return place_as_it_is(ctx, &args).await;
    }
    let mut inputs: Inputs = match &args.vars_file {
        Some(file) => item::parse_document(&text_or_file(None, Some(file), "--vars-file")?)?,
        None => Inputs::new(),
    };
    for assignment in &args.vars {
        let (name, input) = item::parse_assignment(assignment)?;
        inputs.insert(name, input);
    }
    let client = ctx.client().await?;
    check_writable(ctx, client)?;
    let requested_for = async {
        match &args.requested_for {
            Some(user) => refs::person(client, Person::User, user).await.map(Some),
            None => Ok(None),
        }
    };
    let (item, requested_for) =
        tokio::try_join!(find_item(client, &args.item, false), requested_for)?;
    item.check_orderable(client.base())?;
    let dry_run = ctx.globals.dry_run;
    let options = order::Options {
        quantity: args.quantity,
        requested_for,
        allow_unknown: args.allow_unknown_vars,
        unverified: match (args.allow_unverified_formats, dry_run) {
            (true, _) => Unverified::Allow,
            (false, true) => Unverified::Preview,
            (false, false) => Unverified::Refuse,
        },
    };
    let plan = order::prepare(client, &item, &inputs, options).await?;
    for warning in &plan.warnings {
        ctx.out.warn(warning);
    }
    if args.checkout {
        order::check_cart_empty(client).await?;
    }
    let through = if args.checkout {
        " through your cart"
    } else {
        ""
    };
    let what = format!(
        "order \"{}\" (quantity {}){through}",
        plan.item_name, plan.quantity
    );
    let values = fields_text(&plan.shown().into());
    // The client's own preview would show the secrets.
    if dry_run {
        let mut data = plan.to_json();
        data["checkout"] = args.checkout.into();
        return ctx
            .out
            .done(data, &format!("Dry run: would {what}\n\n{values}"));
    }
    if !ctx.globals.yes {
        ctx.out.note(&values);
    }
    ctx.confirm(&what)?;
    let answer = match args.checkout {
        true => order::through_cart(client, &plan).await?,
        false => order::order_now(client, &plan.item_id, plan.body(), &Scrub::of(&plan)).await?,
    };
    order::report(ctx, &plan.item_name, answer, args.checkout, plan.warnings).await
}

/// Refuses before anything is read: reading a cart makes one.
fn check_writable(ctx: &Ctx, client: &Client) -> Result<()> {
    match client.is_read_only() && !ctx.globals.dry_run {
        true => Err(Error::ReadOnly(format!(
            "you asked to change the catalog cart or to order, and profile `{}` is read-only; use another --profile, or look with --dry-run",
            client.profile_name()
        ))),
        false => Ok(()),
    }
}

/// `--data` and `--field`: the body goes out without checks.
async fn place_as_it_is(ctx: &Ctx, args: &OrderArgs) -> Result<()> {
    if !is_sys_id(&args.item) {
        return Err(Error::invalid(format!(
            "you passed the item \"{}\" with --data or --field, which need its sys_id; find it with `sn catalog search`",
            args.item
        )));
    }
    let body = args.body.object()?;
    let client = ctx.client().await?;
    ctx.confirm(&format!("order the item {}", args.item))?;
    let answer = order::order_now(client, &args.item, body.into(), &Scrub::No).await?;
    order::report(ctx, &args.item, answer, false, Vec::new()).await
}

async fn cart(ctx: &Ctx, command: CartCommand) -> Result<()> {
    let client = ctx.client().await?;
    if matches!(command, CartCommand::Show) && ctx.out.raw {
        return show(ctx, client.get(format!("{BASE}/cart")).await?);
    }
    if !matches!(command, CartCommand::Show) {
        check_writable(ctx, client)?;
    }
    let cart = order::cart(client).await?;
    let count = cart.items.len();
    match command {
        CartCommand::Show => ctx.out.item_with(cart.to_json(), json!({}), |_| {
            if cart.items.is_empty() {
                return "Your cart is empty.".to_owned();
            }
            let mut out = format!("Cart {}  {}", cart.id, order::cart_url(client, &cart.id));
            for item in &cart.items {
                out.push_str(&format!(
                    "\n{}  {} x {}",
                    item.id,
                    cell(&item.details, "quantity"),
                    item.name
                ));
            }
            out
        }),
        CartCommand::Submit => {
            if cart.items.is_empty() {
                return Err(Error::invalid(
                    "your cart is empty, so there is nothing to order; add an item with `sn catalog add-to-cart`, or order one with `sn catalog order`",
                ));
            }
            let mut warnings = Vec::new();
            for item in cart.items.iter().filter(|i| i.can_order == Some(false)) {
                let warning = format!(
                    "the instance reports that \"{}\" cannot be ordered as it is, so it may refuse the cart",
                    item.name
                );
                ctx.out.warn(&warning);
                warnings.push(warning);
            }
            ctx.confirm(&format!(
                "order the {count} items in your cart ({})",
                cart.names()
            ))?;
            let scrub = match order::may_hold_secrets(client, &cart).await {
                true => Scrub::Withhold,
                false => Scrub::No,
            };
            order::check_cart_unchanged(client, &cart).await?;
            let answer = order::submit_cart(client, &scrub).await?;
            order::report(ctx, &cart.names(), answer, true, warnings).await
        }
        CartCommand::Remove { cart_item_id } => {
            let item = cart.item(cart_item_id.trim()).ok_or_else(|| {
                Error::NotFound(format!(
                    "the item {cart_item_id} is not in your cart; `sn catalog cart` lists what is"
                ))
            })?;
            ctx.confirm(&format!("remove \"{}\" from your cart", item.name))?;
            match client.delete(format!("{BASE}/cart/{}", item.id)).await {
                Ok(_) => {}
                // The API answers an item that is gone with a plain 400.
                Err(Error::Api(failure)) if failure.status == Some(400) => {
                    return Err(Error::NotFound(format!(
                        "the item {} is no longer in your cart ({})",
                        item.id, failure.message
                    )));
                }
                Err(err) => return Err(err),
            }
            ctx.out.done(
                json!({ "cart_id": cart.id, "cart_item_id": item.id, "item_name": item.name, "removed": true }),
                &format!("Removed \"{}\" from your cart", item.name),
            )
        }
        CartCommand::Clear => {
            if count > 0 {
                ctx.confirm(&format!(
                    "remove all {count} items from your cart ({})",
                    cart.names()
                ))?;
                order::check_cart_unchanged(client, &cart).await?;
                client
                    .delete(format!("{BASE}/cart/{}/empty", cart.id))
                    .await?;
            }
            let text = match count {
                0 => "Your cart is already empty".to_owned(),
                n => format!("Removed {n} items from your cart"),
            };
            ctx.out.done(
                json!({ "cart_id": cart.id, "cleared": true, "items_removed": count }),
                &text,
            )
        }
    }
}

async fn requests(ctx: &Ctx, page: PageArgs, open: bool, display: DisplayArg) -> Result<()> {
    let client = ctx.client().await?;
    let me = caller(client).await?.sys_id;
    let filter = query::all([
        format!("requested_for={me}^ORopened_by={me}").as_str(),
        if open { "active=true" } else { "" },
        "ORDERBYDESCsys_created_on",
    ]);
    // The people's keys in every display mode, to check the rows.
    let keys = ["requested_for.sys_id", "opened_by.sys_id"];
    let mut fields = order::REQUEST_FIELDS.to_vec();
    fields.extend(keys);
    let mut params = TableQuery::new(filter)
        .fields(&fields)
        .limit(page.limit.unwrap_or(SEARCH_LIMIT))
        .display(display.value());
    params.offset = page.offset;
    let found = client.list("sc_req_item", &params).await?;
    let mut rows = found.rows;
    for row in &mut rows {
        let Some(map) = row.as_object_mut() else {
            continue;
        };
        let people: Vec<Value> = keys.iter().filter_map(|key| map.remove(*key)).collect();
        let known: Vec<&str> = people
            .iter()
            .filter_map(|p| p.as_str().or_else(|| p["value"].as_str()))
            .filter(|p| !p.is_empty())
            .collect();
        if known.len() == keys.len() && !known.contains(&me.as_str()) {
            return Err(dropped("requested_for and opened_by", "sc_req_item"));
        }
    }
    let meta = page.meta(rows.len(), found.total, SEARCH_LIMIT);
    let cols = [
        col("NUMBER", "number"),
        flex("ITEM", "cat_item"),
        col("STAGE", "stage"),
        col("STATE", "state"),
        col("CREATED", "sys_created_on"),
    ];
    ctx.out.list(rows, &cols, meta)
}

async fn request_variables(ctx: &Ctx, given: &str, include_empty: bool) -> Result<()> {
    let client = ctx.client().await?;
    let sys_id = super::args::record_of(client, "sc_req_item", given).await?;
    let params = TableQuery::default()
        .fields(&["sys_id", "number", "cat_item"])
        .display(DisplayValue::False);
    let requested = client.record("sc_req_item", &sys_id, &params).await?;
    let item = match raw(&requested, "cat_item") {
        "" => {
            return Err(Error::invalid(format!(
                "you passed {given}, which has no catalog item, so its variables are not known; read them with `sn variables get sc_req_item {sys_id}`"
            )));
        }
        item => read_item(client, item).await?,
    };
    let names: Vec<String> = item
        .readable_names()
        .iter()
        .map(|name| format!("variables.{name}"))
        .collect();
    let values = match names.is_empty() {
        true => Vec::new(),
        false => {
            let params = TableQuery {
                fields: names,
                display: DisplayValue::All,
                ..TableQuery::default()
            };
            let record = client.record("sc_req_item", &sys_id, &params).await?;
            item::ordered(&item, &record)
        }
    };
    let people = ctx.out.human();
    // An order from this output would fill a cleared value with the default.
    let shown: Vec<&item::Ordered> = values
        .iter()
        .filter(|v| include_empty || !v.value.is_empty() || (!people && v.has_default))
        .collect();
    if people {
        let rows = shown
            .iter()
            .map(|v| {
                let value = match v.sensitive {
                    true => REDACTED,
                    false => v.display.as_str(),
                };
                json!({ "name": v.name, "label": v.label, "value": value })
            })
            .collect();
        let cols = [
            col("NAME", "name"),
            col("LABEL", "label"),
            flex("VALUE", "value"),
        ];
        return ctx.out.list(rows, &cols, json!({}));
    }
    let mut out = Map::new();
    let (mut secret, mut fixed, mut unknown) = (Vec::new(), Vec::new(), Vec::new());
    for v in shown {
        match &v.order_value {
            _ if v.sensitive => secret.push(v.name.as_str()),
            _ if v.read_only => fixed.push(v.name.as_str()),
            None => unknown.push(v.name.as_str()),
            Some(value) => {
                out.insert(v.name.clone(), value.as_str().into());
            }
        }
    }
    let mut warnings = Vec::new();
    for (names, why) in [
        (secret, "are sensitive; add them yourself to --vars-file"),
        (fixed, "are read-only; the form fills them"),
        (
            unknown,
            "came without a value in your time zone; see them with --output table",
        ),
    ] {
        if !names.is_empty() {
            let warning = format!("left out {}, which {why}", names.join(", "));
            ctx.out.warn(&warning);
            warnings.push(warning);
        }
    }
    let meta = match warnings.is_empty() {
        true => json!({}),
        false => json!({ "warnings": warnings }),
    };
    ctx.out.item_with(out.into(), meta, fields_text)
}
