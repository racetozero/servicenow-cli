//! Placing orders. Each request that orders something is sent once, and a
//! failure after it was sent says that the order may exist.

use super::BASE;
use super::item::{
    Cart, Check, DATE_FORMAT, Inputs, Item, Outcome, Plan, REDACTED, TIME_FORMAT, UNORDERABLE,
    Validation, validate,
};
use crate::cmd::lookup::PARALLEL;
use crate::ctx::Ctx;
use futures_util::stream::{self, StreamExt, TryStreamExt};
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::error::ApiFailure;
use sn_core::refs::caller;
use sn_core::refs::{self, Person};
use sn_core::table::raw;
use sn_core::{Client, DisplayValue, Error, Result, TableQuery, is_sys_id, query};

pub const MAY_HAVE_ORDERED: &str =
    "the order may have been placed, so check `sn catalog requests mine` before you try again";
pub const MAY_BE_IN_CART: &str = "the item may be in your cart, though nothing was ordered, so check `sn catalog cart` before you try again";
/// Fields of requested items in lists.
pub const REQUEST_FIELDS: &[&str] = &[
    "number",
    "cat_item",
    "stage",
    "state",
    "quantity",
    "requested_for",
    "request",
    "sys_created_on",
    "sys_id",
];

/// What to do when the user's date format cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unverified {
    Refuse,
    Allow,
    /// A dry run says what an order would do.
    Preview,
}

pub struct Options {
    pub quantity: u32,
    pub requested_for: Option<String>,
    pub allow_unknown: bool,
    pub unverified: Unverified,
}

/// Checks an order completely and reports all problems at once.
pub async fn prepare(
    client: &Client,
    item: &Item,
    inputs: &Inputs,
    options: Options,
) -> Result<Plan> {
    let mut checked = validate(item, inputs, options.quantity, options.allow_unknown);
    let checks = std::mem::take(&mut checked.checks);
    let values = checks
        .iter()
        .flat_map(|check| check.values.iter().map(move |value| (check, value)));
    let mut confirmed = stream::iter(values)
        .map(|(check, value)| confirm(client, check, value))
        .buffered(PARALLEL)
        .try_collect::<Vec<_>>()
        .await?
        .into_iter();
    for check in &checks {
        let mut ids = Vec::new();
        let before = checked.problems.len();
        for result in confirmed.by_ref().take(check.values.len()) {
            match result {
                Ok(id) => ids.push(id),
                Err(problem) => checked.problems.push(problem),
            }
        }
        if checked.problems.len() > before {
            continue;
        }
        checked
            .variables
            .insert(check.variable.clone(), ids.join(","));
        if check.has_script() {
            checked.warnings.push(format!(
                "{}: the reference qualifier is a script that cannot be run here; only the table {} was checked",
                check.variable, check.table
            ));
        }
    }
    if checked.uses_dates {
        let times = item.variables.iter().any(|v| {
            v.kind == super::item::Kind::DateTime
                && checked
                    .variables
                    .get(&v.name)
                    .is_some_and(|value| !value.trim().is_empty())
        });
        let (date, time) = formats(client).await;
        check_format(&mut checked, "date", date, DATE_FORMAT, options.unverified);
        if times {
            check_format(&mut checked, "time", time, TIME_FORMAT, options.unverified);
            checked
                .warnings
                .push("times are read in the time zone of your ServiceNow user".into());
        }
    }
    if item.instance_orderable == Some(false) {
        checked.warnings.push(UNORDERABLE.into());
    }
    if item.client_logic {
        checked.warnings.push(
            "this item has catalog UI policies or client scripts, which only the portal runs"
                .into(),
        );
    }
    if !item.widgets.is_empty() {
        checked.warnings.push(format!(
            "this item has portal widgets that cannot be filled here ({}); use the portal if the order is refused or incomplete",
            item.widgets.join(", ")
        ));
    }
    if !checked.problems.is_empty() {
        return Err(Error::Other {
            code: "invalid",
            message: format!(
                "you cannot order \"{}\" like this:\n  - {}",
                item.name,
                checked.problems.join("\n  - ")
            ),
            data: json!({ "problems": checked.problems }),
        });
    }
    let sensitive = item
        .variables
        .iter()
        .filter(|v| v.is_sensitive())
        .map(|v| v.name.clone())
        .collect();
    Ok(Plan {
        item_id: item.sys_id.clone(),
        item_name: item.name.clone(),
        item_class: item.class.clone(),
        quantity: options.quantity,
        requested_for: options.requested_for,
        variables: checked.variables,
        sensitive,
        warnings: checked.warnings,
    })
}

/// Confirms that the value is a record of the variable's table, and one that
/// its reference qualifier allows. The sys_id, or what is wrong.
async fn confirm(
    client: &Client,
    check: &Check,
    value: &str,
) -> Result<std::result::Result<String, String>> {
    let Check {
        variable, table, ..
    } = check;
    let qualifier = check.qualifier.as_deref().filter(|_| !check.has_script());
    let found = match table.as_str() {
        _ if is_sys_id(value) => Ok(value.to_ascii_lowercase()),
        "sys_user" => refs::person(client, Person::User, value).await,
        "sys_user_group" => refs::person(client, Person::Group, value).await,
        _ => Err(Error::invalid(format!(
            "\"{value}\" is not a sys_id, and only users and groups can be named; pass the sys_id of a {table} record"
        ))),
    };
    let id = match found {
        Ok(id) => id,
        // These describe the value; anything else is a failure.
        Err(err @ (Error::Invalid(_) | Error::NotFound(_) | Error::Input { .. })) => {
            return Ok(Err(format!("{variable}: {err}")));
        }
        Err(err) => return Err(err),
    };
    // The lookup by name read the record.
    if qualifier.is_none() && !is_sys_id(value) {
        return Ok(Ok(id));
    }
    // Each `^NQ` part is a query of its own and needs the restriction.
    let only = query::term("sys_id", "=", &id);
    let params = TableQuery::new(query::bounded(qualifier.unwrap_or_default(), &only))
        .fields(&["sys_id"])
        .limit(1)
        .display(DisplayValue::False);
    let rows = client.list(table, &params).await?.rows;
    // A dropped term could match another record.
    let confirmed = rows
        .iter()
        .any(|r| raw(r, "sys_id").eq_ignore_ascii_case(&id));
    Ok(match (confirmed, qualifier) {
        (true, _) => Ok(id),
        (false, Some(_)) => Err(format!(
            "{variable}: \"{value}\" is not a {table} record that the variable's reference qualifier allows"
        )),
        (false, None) => Err(format!(
            "{variable}: there is no {table} record \"{value}\" that you can read"
        )),
    })
}

/// The instance reads dates with the user's own formats, so another format
/// than the one sent would shift them without a word.
fn check_format(
    checked: &mut Validation,
    kind: &str,
    format: Option<String>,
    expected: &str,
    unverified: Unverified,
) {
    let unknown = format!(
        "your ServiceNow {kind} format cannot be read, so a {kind} sent as {expected} may be misread"
    );
    match (format, unverified) {
        (Some(format), _) if format == expected => {}
        (Some(format), _) => checked.problems.push(format!(
            "your ServiceNow {kind} format is {format}, and a {kind} is sent as {expected}, which the instance would misread; set your {kind} format to {expected}, or use the portal"
        )),
        (None, Unverified::Refuse) => checked.problems.push(format!(
            "{unknown}; set your {kind} format to {expected}, or pass --allow-unverified-formats"
        )),
        (None, Unverified::Allow) => checked
            .warnings
            .push(format!("{unknown} (--allow-unverified-formats)")),
        (None, Unverified::Preview) => checked.warnings.push(format!(
            "{unknown}; the order itself needs --allow-unverified-formats"
        )),
    }
}

/// The date and time formats of the user, or else of the instance. `None`
/// when unreadable.
async fn formats(client: &Client) -> (Option<String>, Option<String>) {
    let Ok(me) = caller(client).await else {
        return (None, None);
    };
    let params = TableQuery::default()
        .fields(&["sys_id", "date_format", "time_format"])
        .display(DisplayValue::False);
    let Ok(user) = client.record("sys_user", &me.sys_id, &params).await else {
        return (None, None);
    };
    // A field that is missing was unreadable; an empty one uses the default.
    let own = |field: &str| user.get(field).and_then(Value::as_str);
    let (date, time) = (own("date_format"), own("time_format"));
    let mut defaults = Vec::new();
    if date == Some("") || time == Some("") {
        let params = TableQuery::new("nameINglide.sys.date_format,glide.sys.time_format")
            .fields(&["name", "value"])
            .limit(2)
            .display(DisplayValue::False);
        defaults = client
            .list("sys_properties", &params)
            .await
            .map(|page| page.rows)
            .unwrap_or_default();
    }
    let effective = |own: Option<&str>, property: &str| match own? {
        "" => defaults
            .iter()
            .find(|r| raw(r, "name") == property)
            .map(|r| raw(r, "value").to_owned())
            .filter(|format| !format.is_empty()),
        own => Some(own.to_owned()),
    };
    (
        effective(date, "glide.sys.date_format"),
        effective(time, "glide.sys.time_format"),
    )
}

/// How the instance's error text is made safe: it can quote what was sent.
pub enum Scrub {
    /// Nothing sensitive was sent.
    No,
    /// Take these values out.
    Secrets(Vec<String>),
    /// The cart may hold secrets that are not known here.
    Withhold,
}

impl Scrub {
    pub fn of(plan: &Plan) -> Self {
        match plan.secrets() {
            secrets if secrets.is_empty() => Self::No,
            secrets => Self::Secrets(secrets),
        }
    }

    fn apply(&self, failure: Box<ApiFailure>) -> Box<ApiFailure> {
        let status = failure.status.unwrap_or_default();
        let message = match self {
            Self::No => return failure,
            Self::Withhold => "its message is withheld, because your cart may hold sensitive values that it could quote; open the cart in the portal to see why".to_owned(),
            Self::Secrets(secrets) => {
                let error = &failure.payload["error"];
                match error["message"].as_str() {
                    None => "its answer is withheld, because the order has sensitive values".to_owned(),
                    Some(message) => {
                        let mut text = message.to_owned();
                        if let Some(detail) = error["detail"].as_str().filter(|d| !d.is_empty()) {
                            text = format!("{text}: {detail}");
                        }
                        for secret in secrets {
                            text = text.replace(secret, REDACTED);
                        }
                        // An encoded form of a secret would get through.
                        match text.contains(['\\', '&', '%']) {
                            true => "its message is withheld, because it has encoded text and the order has sensitive values".to_owned(),
                            false => text,
                        }
                    }
                }
            }
        };
        Box::new(ApiFailure {
            status: failure.status,
            message: format!("the instance refused with HTTP {status}: {message}"),
            transaction_id: failure.transaction_id,
            ..ApiFailure::default()
        })
    }
}

/// True when the request may have taken effect.
pub fn is_uncertain(err: &Error) -> bool {
    match err {
        Error::Other { data, .. } => data["uncertain"] == true,
        err => err.status().is_some_and(|status| status >= 500),
    }
}

fn uncertain(code: &'static str, message: String) -> Error {
    Error::Other {
        code,
        message,
        data: json!({ "uncertain": true }),
    }
}

/// Sends a request that must not be repeated. `advice` says what to check
/// when its result is not known.
pub async fn send_once(
    client: &Client,
    request: Request,
    scrub: &Scrub,
    what: &str,
    advice: &str,
) -> Result<Value> {
    let response = match client.send(request).await {
        Ok(response) => response,
        Err(Error::Api(failure)) => {
            let mut failure = scrub.apply(failure);
            if failure.status.is_some_and(|status| status >= 500) {
                failure.message = format!("{}; {advice}", failure.message);
            }
            return Err(Error::Api(failure));
        }
        // Without a connection nothing was sent.
        Err(err @ Error::Network(_)) if err.to_string().contains("client error (Connect)") => {
            return Err(err);
        }
        Err(Error::Network(message)) => {
            return Err(uncertain(
                "network",
                format!("the {what} failed after it was sent ({message}); {advice}"),
            ));
        }
        Err(Error::Other {
            code: "timeout",
            message,
            ..
        }) => {
            return Err(uncertain(
                "timeout",
                format!("the {what} failed after it was sent ({message}); {advice}"),
            ));
        }
        Err(err) => return Err(err),
    };
    response.result().map_err(|_| {
        uncertain(
            "api",
            format!("the answer to the {what} cannot be read; {advice}"),
        )
    })
}

pub async fn cart(client: &Client) -> Result<Cart> {
    Cart::parse(&client.get(format!("{BASE}/cart")).await?)
}

pub fn cart_url(client: &Client, id: &str) -> String {
    format!("{}/sc_cart.do?sys_id={id}", client.base())
}

fn conflict(message: String) -> Error {
    Error::Other {
        code: "conflict",
        message,
        data: Value::Null,
    }
}

fn not_ordered(client: &Client, cart_id: &str, message: String) -> Error {
    Error::Other {
        code: "not_ordered",
        message,
        data: json!({ "cart": { "sys_id": cart_id, "url": cart_url(client, cart_id) } }),
    }
}

/// `--checkout` submits the whole cart, so it starts with an empty one.
pub async fn check_cart_empty(client: &Client) -> Result<()> {
    let cart = cart(client).await?;
    match cart.items.is_empty() {
        true => Ok(()),
        false => Err(conflict(format!(
            "your cart already holds {} items ({}), and --checkout would order them too; look with `sn catalog cart`, then order them with `sn catalog cart submit` or remove them with `sn catalog cart clear`",
            cart.items.len(),
            cart.names()
        ))),
    }
}

/// Reads the cart again just before a write: the portal shares it.
pub async fn check_cart_unchanged(client: &Client, confirmed: &Cart) -> Result<()> {
    let now = cart(client).await?;
    match now.contents() == confirmed.contents() {
        true => Ok(()),
        false => Err(conflict(format!(
            "your cart changed after it was read and now holds {} items ({}); nothing was done, so look with `sn catalog cart` and run the command again",
            now.items.len(),
            now.names()
        ))),
    }
}

/// True when the cart may hold secrets. It does not list variables, so each
/// item's definition is read; one that cannot be read counts as sensitive.
pub async fn may_hold_secrets(client: &Client, cart: &Cart) -> bool {
    let reads = cart.items.iter().map(async |item| {
        let Some(id) = &item.catalog_item else {
            return true;
        };
        match super::read_item(client, id).await {
            // The columns of a multi-row set are not listed.
            Ok(item) => item
                .variables
                .iter()
                .any(|v| v.is_sensitive() || v.kind == super::item::Kind::MultiRowSet),
            Err(_) => true,
        }
    });
    futures_util::future::join_all(reads).await.contains(&true)
}

pub async fn submit_cart(client: &Client, scrub: &Scrub) -> Result<Value> {
    let request = Request::new(Method::POST, format!("{BASE}/cart/submit_order")).json(json!({}));
    send_once(client, request, scrub, "cart submission", MAY_HAVE_ORDERED).await
}

/// Orders through the cart: add the item, then submit a cart with only it.
/// Every refusal says that the item is still in the cart.
pub async fn through_cart(client: &Client, plan: &Plan) -> Result<Value> {
    let scrub = Scrub::of(plan);
    let path = format!("{BASE}/items/{}/add_to_cart", plan.item_id);
    let request = Request::new(Method::POST, path).json(plan.body());
    let added = send_once(client, request, &scrub, "add-to-cart", MAY_BE_IN_CART).await?;
    let cart = Cart::parse(&added).map_err(|err| {
        uncertain(
            "api",
            format!("the instance accepted the item, but its answer cannot be read ({err}); {MAY_BE_IN_CART}"),
        )
    })?;
    let left = |why: String| {
        let message = format!(
            "{why}; nothing was ordered, and the item is still in your cart {} ({})",
            cart.id,
            cart_url(client, &cart.id)
        );
        not_ordered(client, &cart.id, message)
    };
    let only = match cart.items.as_slice() {
        [] => {
            return Err(uncertain(
                "api",
                format!(
                    "the instance accepted the item for the cart {}, but lists no items in it; {MAY_BE_IN_CART}",
                    cart.id
                ),
            ));
        }
        [only] => only,
        _ => {
            return Err(left(format!(
                "your cart changed while the item was added and now holds {} items ({}), so it was not submitted",
                cart.items.len(),
                cart.names()
            )));
        }
    };
    let other = only
        .catalog_item
        .as_deref()
        .is_some_and(|id| !id.eq_ignore_ascii_case(&plan.item_id));
    if other {
        return Err(left(format!(
            "your cart changed while the item was added and now holds \"{}\" instead, so it was not submitted",
            only.name
        )));
    }
    match submit_cart(client, &scrub).await {
        Ok(answer) => Ok(answer),
        Err(err) if is_uncertain(&err) => Err(err),
        Err(err) => {
            // The portal can have ordered or removed the item meanwhile.
            let there = self::cart(client)
                .await
                .is_ok_and(|now| now.item(&only.id).is_some());
            match there {
                true => Err(left(format!(
                    "the item was added to your cart, but the submission failed ({err})"
                ))),
                false => Err(uncertain(
                    "api",
                    format!(
                        "the submission of the cart failed ({err}), and the item is no longer in it, so it was ordered or removed elsewhere; check `sn catalog requests mine` and `sn catalog cart` before you order again"
                    ),
                )),
            }
        }
    }
}

/// Orders an item at once.
pub async fn order_now(client: &Client, id: &str, body: Value, scrub: &Scrub) -> Result<Value> {
    let path = format!("{BASE}/items/{id}/order_now");
    let request = Request::new(Method::POST, path).json(body);
    send_once(client, request, scrub, "order", MAY_HAVE_ORDERED).await
}

/// The request that an answer names, or an error that says what happened
/// instead. `from_cart`: a submission is never meant to answer with a cart.
pub fn placed(client: &Client, answer: &Value, from_cart: bool) -> Result<(String, String)> {
    match Outcome::of(answer) {
        Outcome::Request { sys_id, number } => {
            let label = number.unwrap_or_else(|| format!("request {sys_id}"));
            Ok((sys_id, label))
        }
        Outcome::CartOnly { cart_id } if from_cart => Err(uncertain(
            "api",
            format!(
                "the instance answered the submission with the cart {cart_id} ({}) and not with a request, so it is not clear if it ordered; check `sn catalog requests mine` and `sn catalog cart` before you submit again",
                cart_url(client, &cart_id)
            ),
        )),
        Outcome::CartOnly { cart_id } => {
            let message = format!(
                "the instance accepted the order but made no request: the item is in the cart {cart_id} ({}), and maybe a copy in your portal cart, so nothing was ordered. The instance needs a separate checkout (use `sn catalog order --checkout`), or the item cannot be ordered through the API",
                cart_url(client, &cart_id)
            );
            Err(not_ordered(client, &cart_id, message))
        }
        Outcome::Unknown { keys, number } => {
            let fields = match keys.is_empty() {
                true => "no fields".to_owned(),
                false => format!("fields: {}", keys.join(", ")),
            };
            let named = number
                .map(|n| format!(", though it names the request {n}"))
                .unwrap_or_default();
            Err(uncertain(
                "api",
                format!(
                    "the instance answered the order with neither a request nor a cart ({fields}){named}; {MAY_HAVE_ORDERED}"
                ),
            ))
        }
    }
}

/// Prints a placed order with its requested items. The order exists, so a
/// failed lookup is a warning: an error would invite a second order.
pub async fn report(
    ctx: &Ctx,
    name: &str,
    answer: Value,
    from_cart: bool,
    mut warnings: Vec<String>,
) -> Result<()> {
    let client = ctx.client().await?;
    let (id, label) = placed(client, &answer, from_cart)?;
    let params = TableQuery::new(query::term("request", "=", &id))
        .fields(REQUEST_FIELDS)
        .limit(100)
        .display(DisplayValue::False);
    let items = match client.list("sc_req_item", &params).await {
        Ok(page) => Some(
            page.rows
                .into_iter()
                .filter(|row| raw(row, "request") == id)
                .collect::<Vec<_>>(),
        ),
        Err(err) => {
            let text = format!(
                "the order was placed, but its requested items cannot be read ({err}); do not order again, and check `sn catalog requests mine`"
            );
            ctx.out.warn(&text);
            warnings.push(text);
            None
        }
    };
    let link = |table: &str, id: &str| format!("{}/{table}.do?sys_id={id}", client.base());
    let mut text = format!("Ordered {name}\n{label}  {}", link("sc_request", &id));
    for item in items.iter().flatten() {
        text.push_str(&format!(
            "\n{}  {}",
            raw(item, "number"),
            link("sc_req_item", raw(item, "sys_id"))
        ));
    }
    let data = json!({
        "request": answer,
        "request_id": id,
        "url": link("sc_request", &id),
        "requested_items": items,
    });
    let meta = match warnings.is_empty() {
        true => json!({}),
        false => json!({ "warnings": warnings }),
    };
    ctx.out.item_with(data, meta, |_| text)
}
