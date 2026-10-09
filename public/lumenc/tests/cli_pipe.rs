//! The automation subcommands print row by row into pipes like `| head -1`.
//! A reader that closes early must end the command quietly, not panic it.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};

use serde_json::json;

#[test]
fn find_exits_quietly_when_the_reader_closes_the_pipe() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .args(["find", "--role", "text", "--port", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lumenc");

    let (stream, _) = listener.accept().expect("accept");
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    reader.read_line(&mut request).expect("read request");

    // The reader goes away before the first row is written.
    drop(child.stdout.take());

    let rows: Vec<_> = (0..64)
        .map(|i| json!({"id": i, "role": "text", "label": format!("row {i}")}))
        .collect();
    let response = json!({"jsonrpc": "2.0", "id": 1, "result": {"results": rows}});
    let mut writer = stream;
    writeln!(writer, "{response}").expect("write response");

    let out = child.wait_with_output().expect("wait");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "lumenc find panicked on a closed pipe: {stderr}"
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
}
