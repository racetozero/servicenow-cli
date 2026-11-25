//! Performance Analytics scorecards.

use super::args::{Display, show};
use crate::ctx::Ctx;
use crate::output::{col, flex};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::{DisplayValue, Result};

const PATH: &str = "/api/now/pa/scorecards";

#[derive(Subcommand, Debug)]
pub enum ScoresCommand {
    /// List scorecards.
    #[command(visible_alias = "ls")]
    List(Box<ListScores>),
    /// Add a scorecard to your favorites.
    Favorite {
        /// UUID of the scorecard.
        uuid: String,
    },
    /// Remove a scorecard from your favorites.
    Unfavorite {
        /// UUID of the scorecard.
        uuid: String,
    },
}

const SERIES: &str = "Score data";
const DETAIL: &str = "Detail";

#[derive(Args, Debug)]
pub struct ListScores {
    /// UUIDs, comma-separated.
    #[arg(long, alias = "sysparm-uuid")]
    uuid: Option<String>,
    /// sys_id of a breakdown.
    #[arg(long, alias = "sysparm-breakdown")]
    breakdown: Option<String>,
    /// sys_id of a breakdown relation.
    #[arg(long, alias = "sysparm-breakdown-relation")]
    breakdown_relation: Option<String>,
    /// sys_id of an elements filter.
    #[arg(long, alias = "sysparm-elements-filter")]
    elements_filter: Option<String>,
    /// Which indicators: those displayed on the scorecard (true), or all.
    #[arg(long, alias = "sysparm-display", value_enum, help_heading = DETAIL)]
    display: Option<Display>,
    /// Only your favorites.
    #[arg(long, alias = "sysparm-favorites")]
    favorites: bool,
    /// Only key indicators.
    #[arg(long, alias = "sysparm-key")]
    key: bool,
    /// Only indicators with a target.
    #[arg(long, alias = "sysparm-target")]
    target: bool,
    /// Names or descriptions that contain one of these texts, comma-separated.
    #[arg(long, alias = "sysparm-contains")]
    contains: Option<String>,
    /// sys_ids of tags, comma-separated.
    #[arg(long, alias = "sysparm-tags")]
    tags: Option<String>,
    /// Scorecards per page (at most 100).
    #[arg(long, short = 'n', alias = "sysparm-per-page", default_value_t = 10)]
    per_page: u64,
    /// Page number, from 1.
    #[arg(long, alias = "sysparm-page", default_value_t = 1)]
    page: u64,
    /// Sort field.
    #[arg(long, alias = "sysparm-sortby", value_enum, ignore_case = true)]
    sort_by: Option<SortBy>,
    /// Sort direction.
    #[arg(long, alias = "sysparm-sortdir", value_enum, ignore_case = true)]
    sort_dir: Option<SortDir>,
    /// Labels (true), raw values (false), or both (all).
    #[arg(long, value_enum, alias = "sysparm-display-value")]
    display_value: Option<Display>,
    /// Leave the links of reference fields out.
    #[arg(long, alias = "sysparm-exclude-reference-link", help_heading = DETAIL)]
    exclude_reference_link: bool,
    /// Add the scores over time.
    #[arg(long, alias = "sysparm-include-scores", help_heading = SERIES)]
    include_scores: bool,
    /// First date of the scores (ISO 8601).
    #[arg(long, alias = "sysparm-from", help_heading = SERIES)]
    from: Option<String>,
    /// Last date of the scores (ISO 8601).
    #[arg(long, alias = "sysparm-to", help_heading = SERIES)]
    to: Option<String>,
    /// Keep every Nth score.
    #[arg(long, alias = "sysparm-step", help_heading = SERIES)]
    step: Option<u64>,
    /// Most scores per scorecard (-1: all).
    #[arg(long, aliases = ["sysparm-limit", "setLimit", "setlimit"], allow_hyphen_values = true, help_heading = SERIES)]
    limit: Option<i64>,
    /// Add the breakdowns that exist.
    #[arg(long, alias = "sysparm-include-available-breakdowns", help_heading = DETAIL)]
    include_available_breakdowns: bool,
    /// Add the aggregates that exist.
    #[arg(long, alias = "sysparm-include-available-aggregates", help_heading = DETAIL)]
    include_available_aggregates: bool,
    /// Add real-time scores.
    #[arg(long, alias = "sysparm-include-realtime", help_heading = SERIES)]
    include_realtime: bool,
    /// Add the color scheme of targets.
    #[arg(long, alias = "sysparm-include-target-color-scheme", help_heading = DETAIL)]
    include_target_color_scheme: bool,
    /// Add forecast scores.
    #[arg(long, alias = "sysparm-include-forecast-scores", help_heading = SERIES)]
    include_forecast_scores: bool,
    /// Add trend line scores.
    #[arg(long, alias = "sysparm-include-trendline-scores", help_heading = SERIES)]
    include_trendline_scores: bool,
    /// Add the prediction interval.
    #[arg(long, alias = "sysparm-include-prediction-interval", help_heading = SERIES)]
    include_prediction_interval: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SortBy {
    Value,
    Change,
    #[value(name = "CHANGEPERC")]
    ChangePerc,
    Gap,
    #[value(name = "GAPPERC")]
    GapPerc,
    Name,
    Order,
    Default,
    IndicatorGroup,
    Frequency,
    Target,
    Date,
    Direction,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SortDir {
    Asc,
    Desc,
}

/// The name that the API uses for a value.
fn api_name<V: ValueEnum>(value: Option<V>) -> Option<String> {
    Some(value?.to_possible_value()?.get_name().to_owned())
}

pub async fn run(ctx: &Ctx, command: ScoresCommand) -> Result<()> {
    match command {
        ScoresCommand::List(args) => list(ctx, *args).await,
        ScoresCommand::Favorite { uuid } => favorite(ctx, Method::POST, uuid).await,
        ScoresCommand::Unfavorite { uuid } => favorite(ctx, Method::DELETE, uuid).await,
    }
}

async fn list(ctx: &Ctx, args: ListScores) -> Result<()> {
    let mode = |d: Option<Display>| d.map(|d| DisplayValue::from(d).as_str());
    let mut request = Request::new(Method::GET, PATH)
        .query_opt("sysparm_uuid", args.uuid)
        .query_opt("sysparm_breakdown", args.breakdown)
        .query_opt("sysparm_breakdown_relation", args.breakdown_relation)
        .query_opt("sysparm_elements_filter", args.elements_filter)
        .query_opt("sysparm_display", mode(args.display))
        .query_opt("sysparm_contains", args.contains)
        .query_opt("sysparm_tags", args.tags)
        .query("sysparm_per_page", args.per_page.to_string())
        .query("sysparm_page", args.page.to_string())
        .query_opt("sysparm_sortby", api_name(args.sort_by))
        .query_opt("sysparm_sortdir", api_name(args.sort_dir))
        .query_opt("sysparm_display_value", mode(args.display_value))
        .query_opt("sysparm_from", args.from)
        .query_opt("sysparm_to", args.to)
        .query_opt("sysparm_step", args.step.map(|n| n.to_string()))
        .query_opt("sysparm_limit", args.limit.map(|n| n.to_string()));
    for (name, on) in [
        ("sysparm_favorites", args.favorites),
        ("sysparm_key", args.key),
        ("sysparm_target", args.target),
        (
            "sysparm_exclude_reference_link",
            args.exclude_reference_link,
        ),
        ("sysparm_include_scores", args.include_scores),
        (
            "sysparm_include_available_breakdowns",
            args.include_available_breakdowns,
        ),
        (
            "sysparm_include_available_aggregates",
            args.include_available_aggregates,
        ),
        ("sysparm_include_realtime", args.include_realtime),
        (
            "sysparm_include_target_color_scheme",
            args.include_target_color_scheme,
        ),
        (
            "sysparm_include_forecast_scores",
            args.include_forecast_scores,
        ),
        (
            "sysparm_include_trendline_scores",
            args.include_trendline_scores,
        ),
        (
            "sysparm_include_prediction_interval",
            args.include_prediction_interval,
        ),
    ] {
        if on {
            request = request.query(name, "true");
        }
    }
    let rows = match ctx.client().await?.send(request).await?.result()? {
        Value::Array(rows) if !ctx.out.raw => rows,
        other => return show(ctx, other),
    };
    let meta = match rows.len() as u64 >= args.per_page {
        true => {
            json!({ "next_page": args.page + 1, "next": format!("add --page {}", args.page + 1) })
        }
        false => json!({}),
    };
    let cols = [
        flex("NAME", "name"),
        col("VALUE", "value"),
        col("UNIT", "unit"),
        col("CHANGE", "change"),
        col("TARGET", "target"),
        col("DATE", "scores_date"),
        col("FREQUENCY", "frequency"),
        col("UUID", "uuid"),
    ];
    ctx.out.list(rows, &cols, meta)
}

async fn favorite(ctx: &Ctx, method: Method, uuid: String) -> Result<()> {
    let on = method == Method::POST;
    let mut request = Request::new(method, PATH).query("sysparm_uuid", uuid.as_str());
    if on {
        request = request.json(json!({}));
    }
    let answer = ctx.client().await?.send(request).await?.result()?;
    let text = match on {
        true => format!("Added {uuid} to your favorites"),
        false => format!("Removed {uuid} from your favorites"),
    };
    // Removing answers with an empty body.
    let data = match answer {
        Value::Null => json!({ "uuid": uuid, "favorite": on }),
        other => other,
    };
    ctx.out.done(data, &text)
}
