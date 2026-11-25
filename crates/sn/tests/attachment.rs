//! `attachment` and `xml`.

mod support;

use serde_json::{Value, json};
use std::path::Path;
use support::{Cli, on, query, result};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const ID: &str = "9d385017c611228701d22104cc95c371";
const RECORD: &str = "11111111111111111111111111111111";

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

async fn meta(server: &MockServer, name: &str) {
    on(
        server,
        "GET",
        &format!("/api/now/attachment/{ID}"),
        result(json!({ "sys_id": ID, "file_name": name, "size_bytes": "2048", "table_name": "incident", "table_sys_id": RECORD })),
    )
    .await;
}

async fn file(server: &MockServer, content: &[u8]) {
    Mock::given(method("GET"))
        .and(path(format!("/api/now/attachment/{ID}/file")))
        .and(header("accept", "*/*"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(content.to_vec(), "text/plain"))
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn files_of_a_record_or_of_a_query_are_listed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/attachment"))
        .respond_with(|request: &Request| {
            let q = query(request);
            let of = format!("table_name=incident^table_sys_id={RECORD}");
            match q["sysparm_query"].as_str() {
                "file_nameLIKElog" => assert_eq!(q["sysparm_offset"], "5"),
                other => assert_eq!(
                    other,
                    format!("a=1^{of}^NQb=2^{of}^ORDERBYDESCsys_created_on")
                ),
            }
            result(json!([{ "sys_id": ID, "file_name": "a.log", "size_bytes": "2048", "table_name": "incident", "table_sys_id": RECORD }]))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "attachment",
            "list",
            "incident",
            RECORD,
            "-q",
            "a=1^NQb=2",
            "-o",
            "table",
        ])
        .await
        .ok();
    assert!(run.stdout.contains("2.0 KiB"), "{}", run.stdout);
    let run = cli
        .run(&[
            "attachments",
            "list",
            "-q",
            "file_nameLIKElog",
            "--setlimit",
            "1",
            "--offset",
            "5",
            "--agent",
        ])
        .await
        .ok();
    let envelope = run.envelope();
    assert_eq!(envelope["data"][0]["size_bytes"], "2048");
    assert!(envelope["data"][0].get("size").is_none());
    assert_eq!(envelope["meta"]["next_offset"], 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn files_of_another_record_mean_that_the_filter_was_ignored() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/attachment",
        result(json!([{ "table_sys_id": ID }])),
    )
    .await;
    let run = Cli::basic(&server)
        .run(&["attachment", "list", "incident", RECORD])
        .await;
    assert_eq!(run.code, 1);
}

/// Answers `--all` with `pages`, and with nothing after them.
async fn pages(server: &MockServer, total: Option<&'static str>, pages: Vec<Vec<Value>>) {
    Mock::given(method("GET"))
        .and(path("/api/now/attachment"))
        .respond_with(move |request: &Request| {
            let q = query(request);
            assert_eq!(q["sysparm_limit"], "1000");
            let offset = q.get("sysparm_offset").map_or("0", String::as_str);
            let page: usize = offset.parse::<usize>().expect("offset") / 1000;
            let answer = result(pages.get(page).cloned().unwrap_or_default().into());
            match total {
                Some(total) => answer.insert_header("x-total-count", total),
                None => answer,
            }
        })
        .mount(server)
        .await;
}

fn rows(from: usize, count: usize) -> Vec<Value> {
    (from..from + count)
        .map(|n| json!({ "sys_id": format!("{n}") }))
        .collect()
}

/// Access rules hide files after the limit applied, so a short page is not
/// the last one, and the offset counts files that nobody sees.
#[tokio::test(flavor = "multi_thread")]
async fn all_reads_every_page() {
    for (total, requests) in [(None, 4), (Some("2500"), 3)] {
        let server = MockServer::start().await;
        pages(
            &server,
            total,
            vec![rows(0, 1000), rows(1000, 3), rows(2000, 500)],
        )
        .await;
        let run = Cli::basic(&server)
            .run(&["attachment", "list", "--all"])
            .await
            .ok();
        assert_eq!(run.json().as_array().map(Vec::len), Some(1503));
        let sent = server.received_requests().await.expect("requests").len();
        assert_eq!(sent, requests);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn all_ends_when_the_instance_ignores_the_offset() {
    let server = MockServer::start().await;
    pages(&server, None, vec![rows(0, 1000); 5]).await;
    let run = Cli::basic(&server)
        .run(&["attachment", "list", "--all"])
        .await;
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("sysparm_offset"), "{}", run.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn both_forms_of_upload_stream_the_file() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/attachment/file"))
        .and(query_param("table_name", "incident"))
        .and(query_param("table_sys_id", RECORD))
        .and(query_param("file_name", "report.log"))
        .and(header("content-type", "text/plain"))
        .and(header("content-length", "5"))
        .respond_with(|request: &Request| {
            assert_eq!(request.body, b"hello");
            ResponseTemplate::new(201).set_body_json(json!({ "result": { "sys_id": ID } }))
        })
        .expect(4)
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let file = cli.dir.path().join("report.log");
    std::fs::write(&file, "hello").expect("file");
    let file = file.to_str().expect("path");
    let combined = format!("incident:{RECORD}");
    for args in [
        vec!["incident", RECORD, file],
        vec![&combined, file],
        vec!["--table", "incident", "--record", RECORD, "--file", file],
        vec![
            "--record",
            &combined,
            "--file",
            file,
            "--name",
            "report.log",
        ],
    ] {
        let run = cli
            .run(&[&["attachment", "upload"], &args[..], &["--agent"]].concat())
            .await
            .ok();
        assert_eq!(run.envelope()["data"]["sys_id"], ID);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_upload_sends_nothing() {
    let server = MockServer::start().await;
    let cli = Cli::basic(&server);
    let file = cli.dir.path().join("a.txt");
    std::fs::write(&file, "x").expect("file");
    let file = file.to_str().expect("path");
    let dir = cli.dir.path().to_str().expect("path").to_owned();
    for args in [
        vec!["incident", RECORD, "/no/such/file"],
        vec!["incident", RECORD, &dir],
        vec!["incident", RECORD, file, "--name", "../x"],
        vec![file],
        vec![],
        vec!["incident", file, "--record", RECORD],
    ] {
        let run = cli
            .run(&[&["attachment", "upload"], &args[..]].concat())
            .await;
        assert_eq!(run.code, 3, "{args:?} {}", run.stderr);
    }
    let run = cli
        .run(&[
            "attachment",
            "upload",
            "incident",
            RECORD,
            file,
            "--dry-run",
        ])
        .await
        .ok();
    assert!(run.stdout.contains("a.txt"), "{}", run.stdout);
    support::assert_untouched(&server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_go_to_a_file_a_directory_or_stdout() {
    let server = MockServer::start().await;
    meta(&server, "../../evil.txt").await;
    file(&server, b"\x00binary\xff").await;
    let cli = Cli::basic(&server);
    let out = tempfile::tempdir().expect("dir");
    let dir = out.path().to_str().expect("path");
    let link = format!("{}/api/now/attachment/{ID}/file", server.uri());
    let run = cli
        .run(&["attachment", "download", &link, dir, "--agent"])
        .await
        .ok();
    let data = &run.envelope()["data"];
    assert_eq!(data["bytes"], 8);
    assert_eq!(data["content_type"], "text/plain");
    assert_eq!(
        names(out.path()),
        ["evil.txt"],
        "only the last part of the name, and no temporary file"
    );
    assert_eq!(
        std::fs::read(out.path().join("evil.txt")).expect("file"),
        b"\x00binary\xff"
    );

    let dest = out.path().join("other.bin");
    let dest = dest.to_str().expect("path");
    cli.run(&["attachment", "download", ID, "--out", dest])
        .await
        .ok();
    let run = cli.run(&["attachment", "download", ID, dest]).await;
    assert_eq!(run.code, 3, "an existing file is kept");
    assert!(run.stderr.contains("--force"), "{}", run.stderr);
    cli.run(&["attachment", "download", ID, dest, "--force"])
        .await
        .ok();
    let run = cli.run(&["attachment", "download", ID, "-"]).await.ok();
    assert!(run.stdout.contains("binary"), "{}", run.stdout);
    let run = cli
        .run(&["attachment", "download", ID, "-", "--agent"])
        .await;
    assert_eq!(run.envelope()["error"]["code"], "invalid");
    for bad in [
        "nonsense",
        "https://other.example/api/now/attachment/9d385017c611228701d22104cc95c371",
    ] {
        assert_eq!(cli.run(&["attachment", "download", bad]).await.code, 3);
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_download_is_an_ordinary_file() {
    use std::os::unix::fs::PermissionsExt;
    let server = MockServer::start().await;
    file(&server, b"x").await;
    let out = tempfile::tempdir().expect("dir");
    let plain = out.path().join("plain");
    std::fs::write(&plain, "x").expect("file");
    let dest = out.path().join("got");
    Cli::basic(&server)
        .run(&["attachment", "download", ID, dest.to_str().expect("path")])
        .await
        .ok();
    let mode = |path: &Path| std::fs::metadata(path).expect("meta").permissions().mode();
    assert_eq!(mode(&dest), mode(&plain));
}

/// Sends the head of an answer and a part of its body, then waits.
async fn broken_server(close: bool) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut head = [0; 4096];
                let _ = socket.read(&mut head).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\npartial")
                    .await;
                if !close {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
            });
        }
    });
    format!("http://{address}")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_that_breaks_or_stalls_leaves_nothing_behind() {
    for (close, code) in [(true, "network"), (false, "timeout")] {
        let server = MockServer::start().await;
        let mut cli = Cli::basic(&server);
        cli.env[0].1 = broken_server(close).await;
        let out = tempfile::tempdir().expect("dir");
        let dest = out.path().join("x");
        let run = cli
            .run(&[
                "attachment",
                "download",
                ID,
                dest.to_str().expect("path"),
                "--timeout",
                "1",
                "--agent",
            ])
            .await;
        assert_eq!(run.envelope()["error"]["code"], code, "{}", run.stdout);
        assert_eq!(run.code, 5);
        assert!(names(out.path()).is_empty(), "{:?}", names(out.path()));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_file_needs_a_confirmation() {
    let server = MockServer::start().await;
    meta(&server, "a.log").await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/now/attachment/{ID}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["attachment", "delete", ID, "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    assert!(run.stdout.contains("a.log (2.0 KiB)"), "{}", run.stdout);
    let run = cli
        .run(&["attachment", "delete", ID, "--yes", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["deleted"], true);
}

const UNLOAD: &str =
    "<unload><incident action=\"INSERT_OR_UPDATE\"><sys_id>a</sys_id></incident></unload>\n";

async fn columns(server: &MockServer, table: &str, columns: Value) {
    on(
        server,
        "GET",
        &format!("/api/now/ui/meta/{table}"),
        result(json!({ "columns": columns })),
    )
    .await;
}

async fn unload(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/incident.do"))
        .and(query_param("UNL", ""))
        .and(query_param("sysparm_query", "active=true"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(UNLOAD, "text/xml"))
        .mount(server)
        .await;
}

async fn session(server: &MockServer) {
    on(
        server,
        "GET",
        "/api/now/sg/impersonation/session",
        ResponseTemplate::new(200)
            .insert_header("set-cookie", "JSESSIONID=s; Path=/")
            .set_body_json(json!({ "SessionToken": "csrf-secret" })),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn xml_comes_out_untouched() {
    let server = MockServer::start().await;
    columns(&server, "incident", json!({ "active": {} })).await;
    unload(&server).await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["xml", "export", "incident", "active=true"])
        .await
        .ok();
    assert_eq!(run.stdout, UNLOAD);
    let run = cli
        .run(&["xml", "export", "incident", "active=true", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"], UNLOAD);
    let out = cli.dir.path().join("out.xml");
    let run = cli
        .run(&[
            "xml",
            "export",
            "incident",
            "active=true",
            "--out",
            out.to_str().expect("path"),
            "--agent",
        ])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["bytes"], UNLOAD.len());
    assert_eq!(std::fs::read_to_string(out).expect("file"), UNLOAD);
}

#[tokio::test(flavor = "multi_thread")]
async fn xml_refuses_queries_that_the_instance_would_ignore() {
    let server = MockServer::start().await;
    columns(
        &server,
        "incident",
        json!({ "active": {}, "caller_id": { "reference": "sys_user" } }),
    )
    .await;
    columns(&server, "sys_user", json!({ "name": {} })).await;
    columns(&server, "nothing", json!({})).await;
    let cli = Cli::basic(&server);
    for (table, query, code, text) in [
        ("incident", "active=true^zzz=1", 3, "no column `zzz`"),
        (
            "incident",
            "caller_id.zzz=1",
            3,
            "`sys_user` has no column `zzz`",
        ),
        ("incident", "active.x=1", 3, "not a reference"),
        ("nothing", "active=true", 4, "no table `nothing`"),
        ("incident", " ", 3, "empty query"),
        ("a/b", "active=true", 3, "a/b"),
    ] {
        let run = cli.run(&["xml", "export", table, query]).await;
        assert_eq!(run.code, code, "{query}: {}", run.stderr);
        assert!(run.stderr.contains(text), "{query}: {}", run.stderr);
        assert!(run.stdout.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sign_in_page_is_not_an_export() {
    for answer in [
        ResponseTemplate::new(302).insert_header("location", "/login.do"),
        ResponseTemplate::new(200).set_body_raw("<html>sign in</html>", "text/html;charset=UTF-8"),
    ] {
        let server = MockServer::start().await;
        columns(&server, "incident", json!({ "active": {} })).await;
        on(&server, "GET", "/incident.do", answer).await;
        let run = Cli::basic(&server)
            .run(&["xml", "export", "incident", "active=true"])
            .await;
        assert_eq!(run.code, 2, "{}", run.stderr);
        assert!(run.stdout.is_empty(), "{}", run.stdout);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_exports_through_a_session_that_loaded_a_page() {
    let server = MockServer::start().await;
    columns(&server, "incident", json!({ "active": {} })).await;
    session(&server).await;
    on(&server, "GET", "/home.do", ResponseTemplate::new(200)).await;
    unload(&server).await;
    let mut cli = Cli::empty();
    cli.env.push(("SN_INSTANCE".into(), server.uri()));
    cli.env.push(("SN_TOKEN".into(), "bearer-secret".into()));
    let run = cli
        .run(&["xml", "export", "incident", "active=true"])
        .await
        .ok();
    assert_eq!(run.stdout, UNLOAD);
    let requests = server.received_requests().await.expect("requests");
    let at = |path: &str| {
        requests
            .iter()
            .position(|r| r.url.path() == path)
            .expect(path)
    };
    assert!(at("/home.do") < at("/incident.do"));
    let export = &requests[at("/incident.do")];
    assert!(export.headers.get("authorization").is_none());
    assert_eq!(export.headers["cookie"], "JSESSIONID=s");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_import_uploads_the_form_that_the_page_expects() {
    let server = MockServer::start().await;
    session(&server).await;
    Mock::given(method("POST"))
        .and(path("/sys_upload.do"))
        .respond_with(|request: &Request| {
            let body = String::from_utf8_lossy(&request.body);
            for part in [
                "name=\"sysparm_ck\"",
                "csrf-secret",
                "name=\"sysparm_target\"",
                "name=\"attachFile\"; filename=\"unload.xml\"",
                "<sys_id>a</sys_id>",
            ] {
                assert!(body.contains(part), "{part}: {body}");
            }
            assert_eq!(request.headers["x-usertoken"], "csrf-secret");
            assert_eq!(request.headers["cookie"], "JSESSIONID=s");
            assert!(request.headers.contains_key("authorization"));
            ResponseTemplate::new(200)
        })
        .expect(1)
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run_with(&["xml", "import", "-", "--agent"], Some(UNLOAD))
        .await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    let run = cli
        .run_with(&["xml", "import", "-", "--dry-run"], Some(UNLOAD))
        .await
        .ok();
    assert!(!run.stdout.contains("csrf-secret"), "{}", run.stdout);
    let run = cli
        .run_with(&["xml", "import", "-", "--yes", "--agent"], Some(UNLOAD))
        .await
        .ok();
    assert_eq!(
        run.envelope()["data"],
        json!({ "uploaded": "unload.xml", "bytes": UNLOAD.len(), "confirmed": false })
    );
    assert_eq!(
        cli.run_with(&["xml", "import", "-", "--yes"], Some(" \n"))
            .await
            .code,
        3
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_import_is_an_error() {
    let server = MockServer::start().await;
    session(&server).await;
    on(
        &server,
        "POST",
        "/sys_upload.do",
        ResponseTemplate::new(403),
    )
    .await;
    let run = Cli::basic(&server)
        .run_with(&["xml", "import", "-", "--yes"], Some(UNLOAD))
        .await;
    assert_eq!(run.code, 2, "{}", run.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cookie_profile_imports_with_its_own_session() {
    for (answer, code) in [
        (ResponseTemplate::new(200), 0),
        (
            ResponseTemplate::new(302).insert_header("location", "/login.do"),
            2,
        ),
    ] {
        let server = MockServer::start().await;
        session(&server).await;
        Mock::given(method("POST"))
            .and(path("/sys_upload.do"))
            .and(header("cookie", "JSESSIONID=s"))
            .and(header("x-usertoken", "csrf-secret"))
            .respond_with(answer)
            .expect(1)
            .mount(&server)
            .await;
        let mut cli = Cli::empty();
        cli.env = vec![
            ("SN_INSTANCE".into(), server.uri()),
            ("SN_COOKIE".into(), "JSESSIONID=old".into()),
        ];
        let run = cli
            .run_with(&["xml", "import", "-", "--yes"], Some(UNLOAD))
            .await;
        assert_eq!(run.code, code, "{}", run.stderr);
    }
}
