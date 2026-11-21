//! Exercise the installed command boundary, HTTP contract, and saved recovery state.
//! All provider responses come from a local fixture; no Ollama installation is needed.
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "sisu-cli-flow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }

    fn command(&self, endpoint: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sisu"));
        command
            .env_clear()
            .env("HOME", self.0.join("empty-home"))
            .env("XDG_CONFIG_HOME", self.0.join("empty-config"))
            .current_dir(&self.0)
            .args([
                "--json",
                "--model",
                "fixture-model",
                "--timeout",
                "3",
                "--endpoint",
                endpoint,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn session(&self) -> (PathBuf, Value) {
        let entries: Vec<_> = fs::read_dir(self.0.join(".sisu/sessions"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(entries.len(), 1, "one operation must use one session");
        let directory = entries[0].clone();
        let value = read_json(&directory.join("session.json"));
        (directory, value)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn events(output: &Output) -> Vec<Value> {
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stdout must contain only JSON lines: {line:?}: {e}"))
        })
        .collect()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Response {
    status: u16,
    body: String,
}

fn assistant(text: &str) -> Response {
    Response {
        status: 200,
        body: json!({"message":{"role":"assistant","content":text}, "done":true}).to_string(),
    }
}

fn stream(parts: &[&str]) -> Response {
    Response { status: 200, body: parts.iter().enumerate()
        .map(|(index, text)| json!({"message":{"role":"assistant","content":text},"done":index + 1 == parts.len()}).to_string() + "\n")
        .collect() }
}

#[derive(Debug)]
struct Request {
    path: String,
    body: Value,
}

struct Provider {
    endpoint: String,
    requests: JoinHandle<Vec<Request>>,
}

impl Provider {
    fn start(responses: Vec<Response>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = thread::spawn(move || {
            responses.into_iter().map(|response| {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "CLI never made expected provider request");
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("mock accept failed: {error}"),
                }
            };
            let request = read_request(&mut stream);
            write!(stream, "HTTP/1.1 {} Fixture\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.status, response.body.len(), response.body).unwrap();
            request
        }).collect()
        });
        Self { endpoint, requests }
    }

    fn finish(self) -> Vec<Request> {
        self.requests.join().unwrap()
    }
}

fn read_request(stream: &mut TcpStream) -> Request {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut raw = Vec::new();
    let header_end = loop {
        let mut chunk = [0; 4096];
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "request ended before headers");
        raw.extend_from_slice(&chunk[..read]);
        if let Some(end) = raw.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
        assert!(raw.len() < 64 * 1024, "mock request headers exceeded bound");
    };
    let headers = std::str::from_utf8(&raw[..header_end]).unwrap().to_string();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap_or(0);
    assert!(length <= 1024 * 1024);
    while raw.len() < header_end + length {
        let mut chunk = [0; 4096];
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "request body truncated");
        raw.extend_from_slice(&chunk[..read]);
    }
    Request {
        path: headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .into(),
        body: serde_json::from_slice(&raw[header_end..header_end + length]).unwrap(),
    }
}

#[test]
fn piped_streaming_chat_and_explicit_resume_preserve_history() {
    let workspace = Workspace::new();
    let provider = Provider::start(vec![stream(&["hello ", "world"]), stream(&["follow-up"])]);
    let mut command = workspace.command(&provider.endpoint);
    let mut child = command
        .args(["chat", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"Explain this code:\n    return 42\n")
        .unwrap();
    let first = child.wait_with_output().unwrap();
    assert_success(&first);
    let first_events = events(&first);
    assert!(
        first_events
            .iter()
            .any(|e| e["event"] == "delta" && e["text"] == "hello ")
    );
    assert!(
        first_events
            .iter()
            .any(|e| e["kind"] == "response" && e["data"] == "hello world")
    );
    let (directory, saved) = workspace.session();
    assert_eq!(saved["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        saved["messages"][0]["content"],
        "Explain this code:\n    return 42\n"
    );
    assert_eq!(saved["messages"][1]["role"], "assistant");
    let second = workspace
        .command(&provider.endpoint)
        .args([
            "--quiet",
            "--session",
            saved["id"].as_str().unwrap(),
            "chat",
            "What next?",
        ])
        .output()
        .unwrap();
    assert_success(&second);
    assert_eq!(
        events(&second),
        vec![json!({
            "event":"output", "kind":"response", "data":"follow-up"
        })],
        "quiet continuation must silently restore the session and emit only the response"
    );
    let requests = provider.finish();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|r| r.path == "/api/chat" && r.body["stream"] == true)
    );
    assert_eq!(requests[1].body["messages"].as_array().unwrap().len(), 3);
    assert_eq!(requests[1].body["messages"][1]["content"], "hello world");
    assert_eq!(requests[1].body["messages"][2]["content"], "What next?");
    let continued = read_json(&directory.join("session.json"));
    assert_eq!(continued["messages"].as_array().unwrap().len(), 4);
    let journal = fs::read_to_string(directory.join("events.jsonl")).unwrap();
    assert!(
        journal
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).is_ok())
    );
    assert!(!journal.contains("\u{1b}"));
}

fn valid_plan() -> Value {
    json!({"goal":"Build a CLI", "language":"rust", "check_command":"cargo check", "milestones":[
        {"id":"M1","title":"Build","description":"Create the program","depends_on":[],"done_when":["Compiles"]}
    ]})
}

#[test]
fn invalid_replan_preserves_previous_plan_and_records_diagnostic() {
    let workspace = Workspace::new();
    let invalid = "{\"milestones\":\"bad\"}";
    let provider = Provider::start(vec![
        assistant(&valid_plan().to_string()),
        assistant(invalid),
    ]);
    let initial = workspace
        .command(&provider.endpoint)
        .args(["plan", "Build a CLI"])
        .output()
        .unwrap();
    assert_success(&initial);
    let (directory, saved) = workspace.session();
    assert_eq!(saved["plan"]["milestones"][0]["status"], "Pending");
    // Diagnostics must replace a path atomically instead of following a planted
    // symlink and overwriting a different file.
    #[cfg(unix)]
    let protected = Workspace::new();
    #[cfg(unix)]
    let protected_file = protected.0.join("keep.txt");
    #[cfg(unix)]
    {
        fs::write(&protected_file, "untouched diagnostic target").unwrap();
        std::os::unix::fs::symlink(&protected_file, directory.join("last-invalid-plan.txt"))
            .unwrap();
    }
    let failed = workspace
        .command(&provider.endpoint)
        .args([
            "--session",
            saved["id"].as_str().unwrap(),
            "plan",
            "Change the plan",
        ])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(1));
    assert!(events(&failed).iter().any(|e| {
        e["event"] == "failed"
            && e["message"]
                .as_str()
                .unwrap()
                .contains("Invalid model plan")
    }));
    let after = read_json(&directory.join("session.json"));
    assert_eq!(after["plan"], saved["plan"]);
    assert_eq!(after["last_status"], "failed");
    assert_eq!(
        fs::read_to_string(directory.join("last-invalid-plan.txt")).unwrap(),
        invalid
    );
    #[cfg(unix)]
    {
        assert_eq!(
            fs::read_to_string(&protected_file).unwrap(),
            "untouched diagnostic target"
        );
        assert!(
            !fs::symlink_metadata(directory.join("last-invalid-plan.txt"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    assert_eq!(
        after["messages"].as_array().unwrap().len(),
        3,
        "invalid assistant output must not become conversation history"
    );
    let requests = provider.finish();
    assert!(
        requests
            .iter()
            .all(|r| r.body["format"] == "json" && r.body["stream"] == false)
    );
    let validation = workspace
        .command("http://127.0.0.1:1")
        .args([
            "--session",
            saved["id"].as_str().unwrap(),
            "plan",
            "validate",
        ])
        .output()
        .unwrap();
    assert_success(&validation);
}

#[test]
fn execution_unavailable_has_no_workspace_or_provider_side_effects() {
    let workspace = Workspace::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let output = workspace
        .command(&format!("http://{}", listener.local_addr().unwrap()))
        .arg("run")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let output_events = events(&output);
    assert_eq!(output_events.len(), 1);
    assert_eq!(output_events[0]["event"], "failed");
    assert!(
        output_events[0]["message"]
            .as_str()
            .unwrap()
            .contains("Execution unavailable")
    );
    assert!(!workspace.0.join(".sisu").exists());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn provider_failure_exits_one_and_saves_recoverable_user_message() {
    let workspace = Workspace::new();
    let provider = Provider::start(vec![Response {
        status: 503,
        body: "provider busy".into(),
    }]);
    let output = workspace
        .command(&provider.endpoint)
        .args(["chat", "try this"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        events(&output)
            .iter()
            .any(|e| e["event"] == "failed" && e["message"].as_str().unwrap().contains("503"))
    );
    let (_, saved) = workspace.session();
    assert_eq!(saved["last_status"], "failed");
    assert_eq!(saved["messages"].as_array().unwrap().len(), 1);
    assert_eq!(saved["messages"][0]["role"], "user");
    assert_eq!(provider.finish().len(), 1);
}

#[cfg(unix)]
#[test]
fn sigint_cancels_active_request_and_retains_session() {
    let workspace = Workspace::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let mut child = workspace
        .command(&endpoint)
        .args(["chat", "slow request"])
        .spawn()
        .unwrap();
    // Wait for the real HTTP request, ensuring the operation has started and
    // Tokio's cancellation future has been polled, before delivering SIGINT.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    panic!("CLI never started cancellable HTTP request");
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept failed: {error}"),
        }
    };
    let request = read_request(&mut stream);
    assert_eq!(request.path, "/api/chat");
    let killed = Command::new("/bin/kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert!(events(&output).iter().any(|e| e["event"] == "cancelled"));
    let (_, saved) = workspace.session();
    assert_eq!(saved["last_status"], "cancelled");
    assert_eq!(saved["messages"].as_array().unwrap().len(), 1);
    assert_eq!(saved["messages"][0]["content"], "slow request");
}

#[test]
fn json_argument_errors_exit_two_with_one_failed_record() {
    let workspace = Workspace::new();
    let output = workspace
        .command("http://127.0.0.1:1")
        .arg("--not-a-sisu-option")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let records = events(&output);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "failed");
    assert!(
        records[0]["message"]
            .as_str()
            .unwrap()
            .contains("--not-a-sisu-option")
    );
    assert!(
        output.stderr.is_empty(),
        "JSON parse errors belong in the stdout event stream"
    );
    assert!(!workspace.0.join(".sisu").exists());
}

#[cfg(unix)]
#[test]
fn resumed_session_rejects_symlink_event_journal_without_touching_target() {
    let workspace = Workspace::new();
    let protected = Workspace::new();
    let initial = workspace
        .command("http://127.0.0.1:1")
        .args(["mode", "project"])
        .output()
        .unwrap();
    assert_success(&initial);
    let (directory, saved) = workspace.session();
    let target = protected.0.join("keep.txt");
    fs::write(&target, "untouched journal target").unwrap();
    fs::remove_file(directory.join("events.jsonl")).unwrap();
    std::os::unix::fs::symlink(&target, directory.join("events.jsonl")).unwrap();
    let resumed = workspace
        .command("http://127.0.0.1:1")
        .args(["--session", saved["id"].as_str().unwrap(), "status"])
        .output()
        .unwrap();
    assert_eq!(resumed.status.code(), Some(1));
    let records = events(&resumed);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "failed");
    assert!(records[0]["message"].as_str().unwrap().contains("symlink"));
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "untouched journal target"
    );
}
