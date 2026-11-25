//! Knowledge articles (Knowledge Management API).

use super::args::show;
use crate::ctx::Ctx;
use crate::output::{bold, col, flex};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::table::text;
use sn_core::{Error, Result};
use std::fmt::Write;

const PATH: &str = "/api/sn_km_api/knowledge/articles";
const DEFAULT_LIMIT: u64 = 30;

#[derive(Subcommand, Debug)]
pub enum KnowledgeCommand {
    /// Find articles by text.
    Search {
        /// Text to search for.
        text: String,
        /// Encoded query on the articles, for example workflow_state=published.
        #[arg(long, short = 'q', visible_alias = "query")]
        filter: Option<String>,
        #[command(flatten)]
        list: ListArticles,
    },
    /// List articles: all, the featured, or the most viewed.
    #[command(visible_alias = "ls")]
    List {
        /// Encoded query on the articles.
        #[arg(long, short = 'q', visible_alias = "query", conflicts_with_all = ["featured", "most_viewed"])]
        filter: Option<String>,
        /// Only featured articles.
        #[arg(long, conflicts_with = "most_viewed")]
        featured: bool,
        /// The most viewed articles first.
        #[arg(long)]
        most_viewed: bool,
        #[command(flatten)]
        list: ListArticles,
    },
    /// Read an article.
    #[command(visible_alias = "show")]
    Get {
        /// Number (KB0000011) or sys_id.
        article: String,
        /// Extra fields of the article, comma-separated.
        #[arg(long, short = 'f', value_delimiter = ',')]
        fields: Vec<String>,
        /// Language, for example fr (only with a number).
        #[arg(long, value_name = "CODE")]
        language: Option<String>,
        /// Count this as a view of the article.
        #[arg(long)]
        update_view: bool,
        /// Keep the HTML for people, too.
        #[arg(long, conflicts_with = "text")]
        html: bool,
        /// Plain text instead of HTML in JSON, too.
        #[arg(long)]
        text: bool,
    },
}

#[derive(Args, Debug)]
pub struct ListArticles {
    /// sys_ids of knowledge bases, comma-separated.
    #[arg(long, value_name = "SYS_IDS")]
    kb: Option<String>,
    /// Languages, for example en,fr, or all.
    #[arg(long, value_name = "CODES")]
    language: Option<String>,
    /// Extra fields of the articles, comma-separated.
    #[arg(long, short = 'f', value_delimiter = ',')]
    fields: Vec<String>,
    #[command(flatten)]
    page: super::args::PageArgs,
}

pub async fn run(ctx: &Ctx, command: KnowledgeCommand) -> Result<()> {
    match command {
        KnowledgeCommand::Search { text, filter, list } => {
            let request = Request::new(Method::GET, PATH)
                .query("query", text)
                .query_opt("filter", filter);
            articles(ctx, request, list).await
        }
        KnowledgeCommand::List {
            filter,
            featured,
            most_viewed,
            list,
        } => {
            let path = match (featured, most_viewed) {
                (true, _) => format!("{PATH}/featured"),
                (_, true) => format!("{PATH}/most_viewed"),
                _ => PATH.to_owned(),
            };
            let request = Request::new(Method::GET, path).query_opt("filter", filter);
            articles(ctx, request, list).await
        }
        KnowledgeCommand::Get {
            article,
            fields,
            language,
            update_view,
            html,
            text,
        } => {
            // A search shows `kb_knowledge:SYS_ID`.
            let id = article.trim().trim_prefix("kb_knowledge:");
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return Err(Error::invalid(format!(
                    "you passed the article `{article}`; pass its number (KB0000011) or sys_id"
                )));
            }
            let request = Request::new(Method::GET, format!("{PATH}/{id}"))
                .query_opt("fields", (!fields.is_empty()).then(|| fields.join(",")))
                .query_opt("language", language)
                .query_opt("update_view", update_view.then_some("true"));
            let mut found = ctx.client().await?.send(request).await?.result()?;
            if ctx.out.raw {
                return show(ctx, found);
            }
            if text && let Some(content) = found.get_mut("content") {
                *content = plain(content.as_str().unwrap_or_default()).into();
            }
            ctx.out.item_with(found, json!({}), |found| {
                let content = found["content"].as_str().unwrap_or_default();
                article_text(
                    found,
                    &match html || text {
                        true => content.to_owned(),
                        false => plain(content),
                    },
                    ctx.out.tty,
                )
            })
        }
    }
}

async fn articles(ctx: &Ctx, request: Request, list: ListArticles) -> Result<()> {
    let limit = list.page.limit.unwrap_or(DEFAULT_LIMIT);
    let request = request
        .query_opt(
            "fields",
            (!list.fields.is_empty()).then(|| list.fields.join(",")),
        )
        .query_opt("kb", list.kb)
        .query_opt("language", list.language)
        .query("limit", limit.to_string())
        .query_opt("offset", list.page.offset.map(|n| n.to_string()));
    let mut found = ctx.client().await?.send(request).await?.result()?;
    if ctx.out.raw {
        return show(ctx, found);
    }
    let total = found["meta"]["count"].as_u64();
    let rows = match found.get_mut("articles").map(Value::take) {
        Some(Value::Array(rows)) => rows,
        _ => Vec::new(),
    };
    let meta = list.page.meta(rows.len(), total, DEFAULT_LIMIT);
    let mut cols = vec![
        col("NUMBER", "number"),
        flex("TITLE", "title"),
        col("SCORE", "score"),
    ];
    for field in &list.fields {
        cols.push(crate::output::Col {
            header: field.to_ascii_uppercase().into(),
            key: format!("fields.{field}").into(),
            flex: false,
        });
    }
    ctx.out.list(rows, &cols, meta)
}

fn article_text(article: &Value, content: &str, tty: bool) -> String {
    let title = format!(
        "{}  {}",
        text(article, "number"),
        text(article, "short_description").trim()
    );
    let mut out = format!("{}\n\n{content}", bold(&title, tty));
    let files = article["attachments"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    if !files.is_empty() {
        let _ = write!(out, "\n\n{}", bold("Attachments", tty));
        for file in files {
            let _ = write!(
                out,
                "\n  {}  {}  {}",
                text(file, "sys_id"),
                super::attachment::size(text(file, "size_bytes")),
                text(file, "file_name")
            );
        }
    }
    out
}

/// The text of an HTML page: blocks become lines, list items get a dash.
fn plain(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        push_text(&mut out, &rest[..at]);
        rest = &rest[at..];
        // A lone `<` is text.
        let is_tag = rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || "/!".contains(c));
        let Some(end) = rest.find('>').filter(|_| is_tag) else {
            push_text(&mut out, "<");
            rest = &rest[1..];
            continue;
        };
        let name: String = rest[1..end]
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        let closes = rest.starts_with("</");
        rest = &rest[end + 1..];
        match name.as_str() {
            "script" | "style" if !closes => {
                let close = format!("</{name}");
                let skip = rest.to_ascii_lowercase().find(&close);
                rest = &rest[skip.unwrap_or(rest.len())..];
            }
            "br" => out.push('\n'),
            "li" if !closes => out.push_str("\n- "),
            "td" | "th" if closes => out.push_str("  "),
            "p" | "div" | "tr" | "table" | "ul" | "ol" | "pre" | "section" | "blockquote"
            | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                if !out.ends_with("\n\n") =>
            {
                out.push('\n');
            }
            _ => {}
        }
    }
    push_text(&mut out, rest);
    let mut lines: Vec<&str> = Vec::new();
    for line in out.lines().map(str::trim_end) {
        // One empty line is enough between two blocks.
        if !(line.trim().is_empty() && lines.last().is_none_or(|l| l.is_empty())) {
            lines.push(line.trim());
        }
    }
    lines.join("\n").trim().to_owned()
}

/// Adds text with its entities decoded and its white space as one space.
fn push_text(out: &mut String, text: &str) {
    let mut rest = text;
    let mut space = false;
    while let Some(c) = rest.chars().next() {
        let mut skip = c.len_utf8();
        let mut c = c;
        if c == '&'
            && let Some((end, _)) = rest.char_indices().take(12).find(|(_, c)| *c == ';')
            && let Some(decoded) = entity(&rest[1..end])
        {
            c = decoded;
            skip = end + 1;
        }
        rest = &rest[skip..];
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() && !out.ends_with(['\n', ' ']) {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    if space && !out.is_empty() && !out.ends_with(['\n', ' ']) {
        out.push(' ');
    }
}

pub fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        "copy" => '©',
        _ => {
            let code = name.strip_prefix('#')?;
            let code = match code.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => code.parse().ok()?,
            };
            char::from_u32(code)?
        }
    })
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn html_becomes_text() {
        assert_eq!(
            plain(
                "<p><span style=\"x\"><strong>How to</strong></span></p>\r\n<p>Do&nbsp;this &amp; that:</p><ul><li>one</li><li>two</li></ul><script>var a = '<p>';</script>a &lt; b<br>c &#8364; &unknown;"
            ),
            "How to\n\nDo this & that:\n\n- one\n- two\na < b\nc € &unknown;"
        );
        assert_eq!(plain("1 < 2 and <b>x"), "1 < 2 and x");
        assert_eq!(plain("é&é; <"), "é&é; <");
        assert_eq!(plain(""), "");
    }
}
