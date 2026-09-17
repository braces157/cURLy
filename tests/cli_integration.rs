use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Command, Output},
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use tempfile::TempDir;

fn curly() -> Command {
    Command::new(env!("CARGO_BIN_EXE_curly"))
}

fn run_with_storage(args: &[&str], storage: &TempDir) -> Output {
    curly()
        .args(args)
        .env("CURLY_CONFIG_DIR", storage.path().join("config"))
        .env("CURLY_DATA_DIR", storage.path().join("data"))
        .output()
        .expect("run curly")
}

fn spawn_server<F>(
    requests: usize,
    handler: F,
) -> (String, mpsc::Receiver<Vec<u8>>, thread::JoinHandle<()>)
where
    F: Fn(usize, &[u8], &mut TcpStream, &str) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let address = listener.local_addr().expect("test server address");
    let base = format!("http://{address}");
    let base_for_thread = base.clone();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        for index in 0..requests {
            let (mut stream, _) = listener.accept().expect("accept test request");
            let request = read_request(&mut stream);
            let _ = sender.send(request.clone());
            handler(index, &request, &mut stream, &base_for_thread);
        }
    });
    (base, receiver, handle)
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut expected_total = None;
    loop {
        let count = stream.read(&mut buffer).expect("read request");
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if expected_total.is_none()
            && let Some(headers_end) = find_subsequence(&bytes, b"\r\n\r\n")
        {
            let headers = String::from_utf8_lossy(&bytes[..headers_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            expected_total = Some(headers_end + 4 + content_length);
        }
        if expected_total.is_some_and(|total| bytes.len() >= total) {
            break;
        }
    }
    bytes
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_response(stream: &mut TcpStream, status: &str, headers: &[(&str, String)], body: &[u8]) {
    write_response_to(stream, status, headers, body);
}

fn write_response_to<W: Write>(
    writer: &mut W,
    status: &str,
    headers: &[(&str, String)],
    body: &[u8],
) {
    write!(
        writer,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n",
        body.len()
    )
    .expect("write status");
    for (name, value) in headers {
        write!(writer, "{name}: {value}\r\n").expect("write header");
    }
    write!(writer, "Connection: close\r\n\r\n").expect("finish headers");
    writer.write_all(body).expect("write body");
    writer.flush().expect("flush response");
}

fn spawn_tls_server() -> (String, thread::JoinHandle<()>) {
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("self-signed cert");
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], key)
        .expect("TLS server config");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind TLS server");
    let port = listener.local_addr().expect("TLS server address").port();
    let handle = thread::spawn(move || {
        let (tcp, _) = listener.accept().expect("accept TLS connection");
        let connection = ServerConnection::new(Arc::new(config)).expect("TLS connection");
        let mut tls = StreamOwned::new(connection, tcp);
        let mut request = [0_u8; 4096];
        if tls.read(&mut request).is_ok() {
            write_response_to(
                &mut tls,
                "200 OK",
                &[("Content-Type", "application/json".into())],
                br#"{"secure":true}"#,
            );
        }
    });
    (format!("https://localhost:{port}"), handle)
}

#[test]
fn pipeline_stdout_is_exact_response_bytes() {
    let body = b"alpha\0beta\nwithout-extra-newline".to_vec();
    let response_body = body.clone();
    let (base, _, server) = spawn_server(1, move |_, _, stream, _| {
        write_response(
            stream,
            "200 OK",
            &[("Content-Type", "application/octet-stream".into())],
            &response_body,
        );
    });
    let output = curly()
        .arg(format!("{base}/bytes"))
        .arg("--no-history")
        .output()
        .expect("run curly");
    server.join().expect("server thread");

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, body);
    assert!(output.stderr.is_empty());
}

#[test]
fn no_arguments_prints_help() {
    let output = curly().output().expect("run curly without arguments");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    assert!(combined.contains("Usage:"));
    assert!(combined.contains("curly"));
}

#[test]
fn fail_keeps_body_and_returns_22() {
    let (base, _, server) = spawn_server(1, |_, _, stream, _| {
        write_response(
            stream,
            "503 Service Unavailable",
            &[("Content-Type", "application/json".into())],
            br#"{"error":"down"}"#,
        );
    });
    let output = curly()
        .args([
            format!("{base}/failure"),
            "--fail".into(),
            "--no-history".into(),
        ])
        .output()
        .expect("run curly");
    server.join().expect("server thread");

    assert_eq!(output.status.code(), Some(22));
    assert_eq!(output.stdout, br#"{"error":"down"}"#);
}

#[test]
fn redirects_are_disabled_by_default_and_capped_behind_follow() {
    let (base, _, server) = spawn_server(1, |_, _, stream, base| {
        write_response(
            stream,
            "302 Found",
            &[("Location", format!("{base}/final"))],
            b"redirect-body",
        );
    });
    let output = curly()
        .args([format!("{base}/start"), "--no-history".into()])
        .output()
        .expect("run curly");
    server.join().expect("server thread");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"redirect-body");

    let (base, receiver, server) = spawn_server(2, |index, _, stream, base| {
        if index == 0 {
            write_response(
                stream,
                "302 Found",
                &[("Location", format!("{base}/final"))],
                b"",
            );
        } else {
            write_response(stream, "200 OK", &[], b"final-body");
        }
    });
    let output = curly()
        .args([
            format!("{base}/start"),
            "--follow".into(),
            "--no-history".into(),
        ])
        .output()
        .expect("run curly");
    server.join().expect("server thread");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"final-body");
    let first = receiver.recv().expect("first redirect request");
    let second = receiver.recv().expect("followed request");
    assert!(String::from_utf8_lossy(&first).starts_with("GET /start HTTP/1.1"));
    assert!(String::from_utf8_lossy(&second).starts_with("GET /final HTTP/1.1"));
}

#[test]
fn json_body_defaults_to_post_and_preserves_query_order() {
    let (base, receiver, server) = spawn_server(1, |_, _, stream, _| {
        write_response(stream, "200 OK", &[], b"ok");
    });
    let output = curly()
        .args([
            format!("{base}/items"),
            "--query".into(),
            "a=1".into(),
            "--query".into(),
            "a=2".into(),
            "--json".into(),
            r#"{"name":"example"}"#.into(),
            "--no-history".into(),
        ])
        .output()
        .expect("run curly");
    server.join().expect("server thread");
    assert_eq!(output.status.code(), Some(0));

    let request = receiver.recv().expect("captured request");
    let request = String::from_utf8_lossy(&request);
    assert!(request.starts_with("POST /items?a=1&a=2 HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("content-type: application/json")
    );
    assert!(request.ends_with(r#"{"name":"example"}"#));
}

#[test]
fn total_timeout_returns_transport_exit_code() {
    let (base, _, server) = spawn_server(1, |_, _, stream, _| {
        thread::sleep(Duration::from_millis(250));
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    });
    let output = curly()
        .args([
            format!("{base}/slow"),
            "--timeout".into(),
            "0.05".into(),
            "--no-history".into(),
        ])
        .output()
        .expect("run curly");
    server.join().expect("server thread");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP transport error"));
}

#[test]
fn tls_verification_fails_by_default_and_insecure_is_explicit() {
    let (base, server) = spawn_tls_server();
    let output = curly()
        .args([format!("{base}/secure"), "--no-history".into()])
        .output()
        .expect("run curly");
    server.join().expect("TLS server thread");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP transport error"));

    let (base, server) = spawn_tls_server();
    let output = curly()
        .args([
            format!("{base}/secure"),
            "--insecure".into(),
            "--no-history".into(),
        ])
        .output()
        .expect("run curly");
    server.join().expect("TLS server thread");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, br#"{"secure":true}"#);
}

#[test]
fn basic_and_environment_bearer_authentication_are_sent() {
    let (base, receiver, server) = spawn_server(1, |_, _, stream, _| {
        write_response(stream, "200 OK", &[], b"ok");
    });
    let output = curly()
        .args([
            format!("{base}/basic"),
            "--basic".into(),
            "user:pass".into(),
            "--no-history".into(),
        ])
        .output()
        .expect("run curly");
    server.join().expect("basic auth server");
    assert_eq!(output.status.code(), Some(0));
    let captured = receiver.recv().expect("captured basic request");
    let request = String::from_utf8_lossy(&captured).to_ascii_lowercase();
    assert!(request.contains("authorization: basic dxnlcjpwyxnz"));

    let (base, receiver, server) = spawn_server(1, |_, _, stream, _| {
        write_response(stream, "200 OK", &[], b"ok");
    });
    let output = curly()
        .args([
            format!("{base}/bearer"),
            "--bearer-env".into(),
            "CURLY_TEST_BEARER".into(),
            "--no-history".into(),
        ])
        .env("CURLY_TEST_BEARER", "token-value")
        .output()
        .expect("run curly");
    server.join().expect("bearer auth server");
    assert_eq!(output.status.code(), Some(0));
    let captured = receiver.recv().expect("captured bearer request");
    let request = String::from_utf8_lossy(&captured).to_ascii_lowercase();
    assert!(request.contains("authorization: bearer token-value"));
}

#[test]
fn output_file_is_exact_and_headers_stay_on_stderr() {
    let temp = TempDir::new().expect("temporary output directory");
    let destination = temp.path().join("download.bin");
    let body = b"file\0bytes\nexact".to_vec();
    let response_body = body.clone();
    let (base, _, server) = spawn_server(1, move |_, _, stream, _| {
        write_response(
            stream,
            "201 Created",
            &[("X-Test", "header-value".into())],
            &response_body,
        );
    });
    let output = curly()
        .arg(format!("{base}/download"))
        .arg("--output")
        .arg(&destination)
        .arg("--headers")
        .arg("--no-history")
        .output()
        .expect("run curly");
    server.join().expect("output server");

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(destination).unwrap(), body);
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    assert!(stderr.contains("http 201"));
    assert!(stderr.contains("x-test: header-value"));
}

#[test]
fn history_write_failure_warns_without_replacing_success() {
    let temp = TempDir::new().expect("temporary storage");
    let invalid_data_root = temp.path().join("not-a-directory");
    std::fs::write(&invalid_data_root, b"file").unwrap();
    let (base, _, server) = spawn_server(1, |_, _, stream, _| {
        write_response(stream, "200 OK", &[], b"ok");
    });
    let output = curly()
        .arg(format!("{base}/history-warning"))
        .arg("--history")
        .env("CURLY_CONFIG_DIR", temp.path().join("config"))
        .env("CURLY_DATA_DIR", &invalid_data_root)
        .output()
        .expect("run curly");
    server.join().expect("history warning server");

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"ok");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .to_ascii_lowercase()
            .contains("warning: could not write history")
    );
}

#[test]
fn history_replay_executes_complete_request_and_records_replay() {
    let storage = TempDir::new().expect("temporary storage");
    let (base, receiver, server) = spawn_server(2, |_, _, stream, _| {
        write_response(stream, "200 OK", &[], b"replayed");
    });

    let first = run_with_storage(&[&format!("{base}/replay"), "--history"], &storage);
    assert_eq!(first.status.code(), Some(0));
    let replay = run_with_storage(&["history", "replay", "1", "--history"], &storage);
    assert_eq!(replay.status.code(), Some(0));
    assert_eq!(replay.stdout, b"replayed");
    server.join().expect("history replay server");
    assert!(receiver.recv().is_ok());
    assert!(receiver.recv().is_ok());

    let history = run_with_storage(&["history", "list"], &storage);
    assert_eq!(history.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&history.stdout).lines().count(), 2);
}

#[test]
fn saved_request_executes_again_and_history_is_queryable() {
    let storage = TempDir::new().expect("temporary storage");
    let (base, receiver, server) = spawn_server(2, |_, _, stream, _| {
        write_response(stream, "200 OK", &[], b"saved-ok");
    });

    let first = run_with_storage(
        &[&format!("{base}/saved"), "--save", "sample", "--history"],
        &storage,
    );
    assert_eq!(first.status.code(), Some(0));
    assert_eq!(first.stdout, b"saved-ok");

    let second = run_with_storage(&["run", "sample", "--history"], &storage);
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(second.stdout, b"saved-ok");
    server.join().expect("server thread");
    assert!(receiver.recv().is_ok());
    assert!(receiver.recv().is_ok());

    let saved_path = storage
        .path()
        .join("config")
        .join("requests")
        .join("sample.toml");
    assert!(Path::new(&saved_path).is_file());

    let history = run_with_storage(&["history", "list"], &storage);
    assert_eq!(history.status.code(), Some(0));
    let history = String::from_utf8_lossy(&history.stdout);
    assert_eq!(history.lines().count(), 2);
    assert!(history.contains("GET"));
    assert!(history.contains(&format!("{base}/saved")));
}
