//! Files on records. Transfers are streamed both ways.

use super::args::{PageArgs, dropped, show};
use crate::ctx::Ctx;
use crate::output::{col, flex};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use sn_core::client::{Body, Method, Request};
use sn_core::refs::{self, Record};
use sn_core::table::text;
use sn_core::{Client, Error, Result, is_sys_id, query};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PATH: &str = "/api/now/attachment";
const DEFAULT_LIMIT: u64 = 100;
const ALL_PAGE: u64 = 1000;
/// A transfer may take this long; a download that stands still does not.
const TRANSFER: Duration = Duration::from_secs(24 * 3600);

#[derive(Subcommand, Debug)]
pub enum AttachmentCommand {
    /// List the files of a record, or all files that a query matches.
    #[command(visible_alias = "ls")]
    List(ListFiles),
    /// Read the metadata of a file.
    Get {
        /// sys_id of the attachment, or its link.
        attachment: String,
    },
    /// Attach a file to a record: TABLE RECORD FILE, or RECORD FILE.
    Upload(Upload),
    /// Download a file.
    Download(Download),
    /// Delete a file.
    #[command(visible_alias = "rm")]
    Delete {
        /// sys_id of the attachment, or its link.
        attachment: String,
    },
}

#[derive(Args, Debug)]
pub struct ListFiles {
    /// Table, or the record itself: a number, table:number, table:sys_id, or a link.
    #[arg(value_name = "TABLE|RECORD")]
    table: Option<String>,
    /// sys_id or number when the first argument is a table.
    #[arg(value_name = "SYS_ID|NUMBER")]
    record: Option<String>,
    /// Encoded query on the files, for example file_nameLIKElog.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    query: Option<String>,
    #[command(flatten)]
    page: PageArgs,
    /// Read all pages.
    #[arg(long)]
    all: bool,
}

#[derive(Args, Debug)]
pub struct Upload {
    /// TABLE RECORD FILE, or RECORD FILE (a number, table:number, or a link).
    #[arg(value_name = "TABLE RECORD FILE", num_args = 0..=3)]
    words: Vec<String>,
    /// Table of the record.
    #[arg(long)]
    table: Option<String>,
    /// Record: sys_id, number, table:number, or a link.
    #[arg(long)]
    record: Option<String>,
    /// File to upload.
    #[arg(long)]
    file: Option<PathBuf>,
    /// Name of the attachment (default: the file's name).
    #[arg(long, visible_alias = "file-name")]
    name: Option<String>,
    /// Content type (default: from the file's extension).
    #[arg(long, value_name = "TYPE")]
    content_type: Option<String>,
    /// sys_id of an encryption context.
    #[arg(long, value_name = "SYS_ID")]
    encryption_context: Option<String>,
}

#[derive(Args, Debug)]
pub struct Download {
    /// sys_id of the attachment, or its link.
    attachment: String,
    /// File, directory, or - for stdout (default: the attachment's name, here).
    dest: Option<PathBuf>,
    /// The same as the second argument.
    #[arg(long, value_name = "PATH", conflicts_with = "dest")]
    out: Option<PathBuf>,
    /// Replace a file that exists.
    #[arg(long)]
    force: bool,
}

pub async fn run(ctx: &Ctx, command: AttachmentCommand) -> Result<()> {
    match command {
        AttachmentCommand::List(args) => list(ctx, args).await,
        AttachmentCommand::Get { attachment } => {
            let client = ctx.client().await?;
            let id = attachment_id(client, &attachment)?;
            show(ctx, client.get(format!("{PATH}/{id}")).await?)
        }
        AttachmentCommand::Upload(args) => upload(ctx, args).await,
        AttachmentCommand::Download(args) => download(ctx, args).await,
        AttachmentCommand::Delete { attachment } => delete(ctx, &attachment).await,
    }
}

async fn list(ctx: &Ctx, args: ListFiles) -> Result<()> {
    let client = ctx.client().await?;
    let record = match &args.table {
        Some(table) => Some(refs::record(client, table, args.record.as_deref()).await?),
        None => None,
    };
    let own = args.query.as_deref().unwrap_or_default();
    let filter = match &record {
        None => own.to_owned(),
        Some(record) => {
            let of = query::all([
                query::term("table_name", "=", &record.table).as_str(),
                &query::term("table_sys_id", "=", &record.sys_id),
            ]);
            query::ordered(&query::bounded(own, &of), "ORDERBYDESCsys_created_on")
        }
    };
    let mut rows: Vec<Value> = Vec::new();
    let mut page = args.page.clone();
    if args.all {
        page.limit = Some(ALL_PAGE);
    }
    let total = loop {
        let request = Request::new(Method::GET, PATH)
            .query_opt("sysparm_query", (!filter.is_empty()).then_some(&*filter));
        let response = client.send(page.apply(request, DEFAULT_LIMIT)).await?;
        let total = response.total();
        let Value::Array(found) = response.result()? else {
            break total;
        };
        let next = page.offset.unwrap_or(0) + ALL_PAGE;
        // Access rules hide files after the limit applied, so a short page
        // is not the last one.
        let last = found.is_empty() || total.is_some_and(|total| next >= total);
        let repeated = rows
            .first()
            .zip(found.first())
            .is_some_and(|(first, again)| text(first, "sys_id") == text(again, "sys_id"));
        if repeated {
            return Err(Error::api(
                "you passed --all, but the instance ignored sysparm_offset and answered with the first page again; narrow the list with a record or --query, or leave --all out",
                Value::Null,
            ));
        }
        rows.extend(found);
        if !args.all || last {
            break total;
        }
        page.offset = Some(next);
    };
    if let Some(record) = &record
        && rows
            .iter()
            .any(|r| text(r, "table_sys_id") != record.sys_id)
    {
        return Err(dropped("table_sys_id", "sys_attachment"));
    }
    if ctx.out.raw {
        return show(ctx, rows.into());
    }
    let meta = match args.all {
        true => json!({}),
        false => args.page.meta(rows.len(), total, DEFAULT_LIMIT),
    };
    if ctx.out.human() {
        for row in &mut rows {
            let size = size(text(row, "size_bytes"));
            if let Some(map) = row.as_object_mut() {
                map.insert("size".into(), size.into());
            }
        }
    }
    let mut cols = vec![
        flex("FILE", "file_name"),
        col("TYPE", "content_type"),
        col("SIZE", "size"),
        col("BY", "sys_created_by"),
        col("CREATED", "sys_created_on"),
        col("SYS_ID", "sys_id"),
    ];
    if !ctx.out.human() {
        cols[2] = col("SIZE", "size_bytes");
    }
    if record.is_none() {
        cols.insert(1, col("TABLE", "table_name"));
    }
    ctx.out.list(rows, &cols, meta)
}

async fn upload(ctx: &Ctx, args: Upload) -> Result<()> {
    let mut words = args.words;
    let file = match args.file {
        Some(file) => file,
        None => words.pop().map(PathBuf::from).ok_or_else(|| Error::Input {
            arg: "file".into(),
            message: "the file is missing; pass TABLE RECORD FILE, RECORD FILE, or --file".into(),
            choices: Vec::new(),
        })?,
    };
    let (table, id) = match (args.record, args.table, words.as_slice()) {
        (Some(record), Some(table), []) => (table, Some(record)),
        (Some(record), None, []) => (record, None),
        (None, None, [table, record]) => (table.clone(), Some(record.clone())),
        (None, None, [record]) => (record.clone(), None),
        (None, Some(table), [record]) => (table, Some(record.clone())),
        _ => {
            return Err(Error::Input {
                arg: "record".into(),
                message: "the record is missing or was passed twice; pass TABLE RECORD FILE, RECORD FILE, or --table, --record and --file".into(),
                choices: Vec::new(),
            });
        }
    };
    // Found out before anything is sent.
    let meta = std::fs::metadata(&file)
        .and_then(|meta| std::fs::File::open(&file).map(|_| meta))
        .map_err(|err| {
            Error::invalid(format!(
                "you passed the file {}, which cannot be read ({err}); pass a file you can read",
                file.display()
            ))
        })?;
    if !meta.is_file() {
        return Err(Error::invalid(format!(
            "you passed {}, which is not a file; pass a file",
            file.display()
        )));
    }
    let name = match args.name {
        Some(name) => name,
        None => file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    check_file_name(&name)?;
    let content_type = args
        .content_type
        .unwrap_or_else(|| content_type(&file).to_owned());
    let client = ctx.client().await?;
    let Record { table, sys_id } = refs::record(client, &table, id.as_deref()).await?;
    let request = Request::new(Method::POST, format!("{PATH}/file"))
        .query("table_name", table.as_str())
        .query("table_sys_id", sys_id.as_str())
        .query("file_name", name.as_str())
        .query_opt("encryption_context", args.encryption_context)
        .body(Body::File {
            path: file,
            content_type,
        })
        .timeout(ctx.globals.timeout.map_or(TRANSFER, Duration::from_secs));
    let made = client.send(request).await?.result()?;
    let done = format!(
        "Uploaded {name} ({}) to {table} {sys_id}",
        size(&meta.len().to_string())
    );
    ctx.out.done(made, &done)
}

async fn download(ctx: &Ctx, args: Download) -> Result<()> {
    let client = ctx.client().await?;
    let id = attachment_id(client, &args.attachment)?;
    let dest = args.dest.or(args.out);
    let to_stdout = dest.as_deref() == Some(Path::new("-"));
    if to_stdout && ctx.out.agent {
        return Err(Error::invalid(
            "you passed - with --agent, but agent mode prints one JSON envelope; pass a file name instead",
        ));
    }
    // Only a directory needs the attachment's own name.
    let dest = match dest {
        Some(path) if !path.is_dir() => path,
        directory => {
            let meta = client.get(format!("{PATH}/{id}")).await?;
            let name = safe_name(text(&meta, "file_name"))?;
            directory.unwrap_or_default().join(name)
        }
    };
    if !to_stdout {
        check_destination(&dest, args.force)?;
    }
    let mut response = open(ctx, Request::new(Method::GET, format!("{PATH}/{id}/file"))).await?;
    if to_stdout {
        let bytes = copy(ctx, &mut response, &mut std::io::stdout().lock()).await?;
        ctx.out
            .note(&format!("Downloaded {}", size(&bytes.to_string())));
        return Ok(());
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = save(ctx, &mut response, &dest, args.force).await?;
    ctx.out.done(
        json!({ "attachment": id, "path": dest, "bytes": bytes, "content_type": content_type }),
        &format!(
            "Downloaded {} ({})",
            dest.display(),
            size(&bytes.to_string())
        ),
    )
}

async fn delete(ctx: &Ctx, attachment: &str) -> Result<()> {
    let client = ctx.client().await?;
    let id = attachment_id(client, attachment)?;
    let meta = client.get(format!("{PATH}/{id}")).await?;
    let name = text(&meta, "file_name");
    let of = format!(
        "{} {}",
        text(&meta, "table_name"),
        text(&meta, "table_sys_id")
    );
    ctx.confirm(&format!(
        "delete the attachment {name} ({}) of {of}",
        size(text(&meta, "size_bytes"))
    ))?;
    client.delete(format!("{PATH}/{id}")).await?;
    ctx.out.done(
        json!({
            "attachment": id, "file_name": name, "table": meta["table_name"],
            "table_sys_id": meta["table_sys_id"], "deleted": true,
        }),
        &format!("Deleted {name} of {of}"),
    )
}

/// A sys_id, or a link to the file or its metadata.
fn attachment_id(client: &Client, value: &str) -> Result<String> {
    let bad = || {
        Error::invalid(format!(
            "you passed `{value}`, which does not name an attachment; pass its sys_id (see `sn attachment list`) or its link on {}",
            client.host()
        ))
    };
    if is_sys_id(value) {
        return Ok(value.to_ascii_lowercase());
    }
    let url = url::Url::parse(value).map_err(|_| bad())?;
    let base = url::Url::parse(client.base()).map_err(|_| bad())?;
    if url.origin() != base.origin() {
        return Err(bad());
    }
    let after = url
        .path_segments()
        .into_iter()
        .flatten()
        .skip_while(|s| *s != "attachment")
        .nth(1)
        .map(str::to_owned);
    after
        .or_else(|| {
            url.query_pairs()
                .find(|(name, _)| name == "sys_id")
                .map(|(_, id)| id.into_owned())
        })
        .filter(|id| is_sys_id(id))
        .map(|id| id.to_ascii_lowercase())
        .ok_or_else(bad)
}

fn idle(ctx: &Ctx) -> Duration {
    Duration::from_secs(ctx.globals.timeout.unwrap_or(60))
}

fn stalled(limit: Duration) -> Error {
    Error::Other {
        code: "timeout",
        message: format!(
            "the instance sent nothing for {} seconds; run the command again, or raise the limit with --timeout SECONDS",
            limit.as_secs()
        ),
        data: Value::Null,
    }
}

/// Opens a download. The time limit is for each read, not for the whole.
pub async fn open(ctx: &Ctx, request: Request) -> Result<reqwest::Response> {
    let request = request.timeout(TRANSFER).header("accept", "*/*");
    // The client may try four times before it has an answer.
    let limit = idle(ctx) * 4;
    tokio::time::timeout(limit, ctx.client().await?.open(request))
        .await
        .map_err(|_| stalled(limit))?
}

/// Copies a download as it arrives.
pub async fn copy(
    ctx: &Ctx,
    response: &mut reqwest::Response,
    sink: &mut impl Write,
) -> Result<u64> {
    let failed = |err: std::io::Error| {
        Error::Internal(format!(
            "cannot write the download ({err}); check the disk space and the path"
        ))
    };
    let limit = idle(ctx);
    let mut bytes = 0;
    loop {
        let chunk = tokio::time::timeout(limit, response.chunk())
            .await
            .map_err(|_| stalled(limit))??;
        let Some(chunk) = chunk else { break };
        match sink.write_all(&chunk) {
            Ok(()) => bytes += chunk.len() as u64,
            // A closed pipe (`| head`) is not an error worth reporting.
            Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => return Ok(bytes),
            Err(err) => return Err(failed(err)),
        }
    }
    sink.flush().map_err(failed)?;
    Ok(bytes)
}

/// Refuses a destination before anything is downloaded.
pub fn check_destination(dest: &Path, force: bool) -> Result<()> {
    if dest.exists() && !force {
        return Err(Error::invalid(format!(
            "you passed {}, which exists already; pass another path, or --force to replace it",
            dest.display()
        )));
    }
    match dest.file_name().is_some() && directory(dest).is_dir() {
        true => Ok(()),
        false => Err(Error::invalid(format!(
            "you passed {}, whose directory does not exist; pass a path in a directory that exists",
            dest.display()
        ))),
    }
}

fn directory(dest: &Path) -> &Path {
    match dest.parent() {
        Some(parent) if parent != Path::new("") => parent,
        _ => Path::new("."),
    }
}

/// Downloads next to the destination and renames at the end, so the
/// destination never holds a part of a file. A run that stops removes it.
pub async fn save(
    ctx: &Ctx,
    response: &mut reqwest::Response,
    dest: &Path,
    force: bool,
) -> Result<u64> {
    let failed = |err: &std::io::Error| {
        Error::invalid(format!(
            "cannot write {} ({err}); pass a path you can write",
            dest.display()
        ))
    };
    let mut builder = tempfile::Builder::new();
    // The default is a private file; a download is an ordinary one.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut file = builder
        .prefix(".sn-")
        .suffix(".part")
        .tempfile_in(directory(dest))
        .map_err(|err| failed(&err))?;
    let bytes = copy(ctx, response, &mut file).await?;
    file.as_file().sync_all().map_err(|err| failed(&err))?;
    let kept = match force {
        true => file.persist(dest),
        false => file.persist_noclobber(dest),
    };
    kept.map_err(|err| failed(&err.error))?;
    Ok(bytes)
}

/// The last part of a name that the server sent.
fn safe_name(name: &str) -> Result<&str> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    check_file_name(name)?;
    // `C:x` and `x:stream` are not plain names on Windows.
    match cfg!(windows) && name.contains(['<', '>', ':', '"', '|', '?', '*']) {
        true => Err(Error::invalid(format!(
            "the attachment is named `{name}`, which is not a file name on Windows; pass a destination"
        ))),
        false => Ok(name),
    }
}

fn check_file_name(name: &str) -> Result<()> {
    let bad = matches!(name.trim(), "" | "." | "..")
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control);
    match bad {
        true => Err(Error::invalid(format!(
            "the file name `{}` is not usable; pass a name without slashes and control characters",
            name.escape_debug()
        ))),
        false => Ok(()),
    }
}

/// `1.5 MiB` for a number of bytes.
pub fn size(bytes: &str) -> String {
    let Ok(count) = bytes.parse::<u64>() else {
        return bytes.to_owned();
    };
    if count < 1024 {
        return format!("{count} B");
    }
    let mut value = count as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    format!("{value:.1} {unit}")
}

fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "txt" | "log" | "md" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "zip" => "application/zip",
        "gz" | "gzip" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "eml" => "message/rfc822",
        "msg" => "application/vnd.ms-outlook",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_names_become_plain_names() {
        for (given, name) in [
            ("report.pdf", "report.pdf"),
            ("../../etc/passwd", "passwd"),
            ("C:\\Users\\a\\b.txt", "b.txt"),
            (" a b.txt ", "a b.txt"),
        ] {
            assert_eq!(safe_name(given).expect(given), name);
        }
        for bad in ["", "..", "a/..", "x/", "a\nb", "a\0b"] {
            assert!(safe_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(size("0"), "0 B");
        assert_eq!(size("1023"), "1023 B");
        assert_eq!(size("1536"), "1.5 KiB");
        assert_eq!(size("5368709120"), "5.0 GiB");
        assert_eq!(size("unknown"), "unknown");
    }
}
