use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use sisu_core::{
    LlmConfig, LlmError, MessageRole, ModelClient, ModelMessage, execute_milestones, load_plan,
    parse_project_plan, persist_plan,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

fn plan_json() -> Value {
    json!({"goal":"Build a CLI", "language":"rust", "check_command":"cargo check", "milestones":[
        {"id":"M2","title":"Test", "description":"Test CLI", "depends_on":["M1"], "done_when":["Tests pass"]},
        {"id":"M1","title":"Build", "description":"Build CLI", "depends_on":[], "done_when":["CLI works"]}
    ]})
}

fn temporary() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sisu-core-foundation-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn validates_dependencies_independently_of_plan_order() {
    let plan = parse_project_plan(&format!("```json\n{}\n```", plan_json())).unwrap();
    plan.validate().unwrap();
}

#[test]
fn rejects_duplicate_missing_self_and_cyclic_dependencies() {
    for mutate in [0, 1, 2, 3, 4, 5] {
        let mut plan = plan_json();
        match mutate {
            0 => plan["milestones"][1]["id"] = json!("M2"),
            1 => plan["milestones"][0]["depends_on"] = json!(["missing"]),
            2 => plan["milestones"][0]["depends_on"] = json!(["M2"]),
            3 => plan["milestones"][1]["depends_on"] = json!(["M2"]),
            4 => plan["milestones"][0]["depends_on"] = json!(["M1", "M1"]),
            _ => plan["milestones"][0]["done_when"] = json!([]),
        }
        assert!(
            parse_project_plan(&plan.to_string()).is_err(),
            "accepted invalid case {mutate}"
        );
    }
}

#[test]
fn atomic_persistence_round_trips_and_excludes_overlapping_writers() {
    let directory = temporary();
    let mut plan = parse_project_plan(&plan_json().to_string()).unwrap();
    let saved = persist_plan(&directory, &plan).unwrap();
    let original = fs::read(&saved).unwrap();
    assert_eq!(
        serde_json::to_value(load_plan(&directory).unwrap()).unwrap(),
        serde_json::to_value(&plan).unwrap()
    );
    let lock_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("milestones.json.lock"))
        .unwrap();
    lock_file.try_lock().unwrap();
    plan.goal = "A different goal".into();
    assert!(persist_plan(&directory, &plan).is_err());
    assert_eq!(fs::read(&saved).unwrap(), original);
    drop(lock_file);
    // The advisory lock file stays on disk and can immediately be reused.
    persist_plan(&directory, &plan).unwrap();
    let original = fs::read(&saved).unwrap();
    plan.milestones[0].depends_on = vec!["unknown".into()];
    assert!(persist_plan(&directory, &plan).is_err());
    assert_eq!(fs::read(&saved).unwrap(), original);
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn execution_without_sandbox_preserves_milestone_state() {
    let mut plan = parse_project_plan(&plan_json().to_string()).unwrap();
    let original = serde_json::to_value(&plan).unwrap();
    assert!(
        execute_milestones("unused", "unused", &mut plan)
            .await
            .is_err()
    );
    assert_eq!(serde_json::to_value(plan).unwrap(), original);
}

/// A tiny HTTP fixture. Capture the request and split a response into network
/// writes, including boundaries within UTF-8 codepoints.
async fn mock(
    body: Vec<u8>,
    status: &str,
    split: Option<usize>,
    delay: Duration,
) -> (String, oneshot::Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let status = status.to_string();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0u8; 4096];
            let count = socket.read(&mut chunk).await.unwrap();
            if count == 0 {
                return;
            }
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let header = String::from_utf8_lossy(&bytes[..header_end]);
        let length: usize = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap_or(0);
        while bytes.len() < header_end + length {
            let mut chunk = [0u8; 4096];
            let count = socket.read(&mut chunk).await.unwrap();
            if count == 0 {
                return;
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        let request =
            serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap_or(Value::Null);
        let _ = tx.send(request);
        tokio::time::sleep(delay).await;
        let headers = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        if socket.write_all(headers.as_bytes()).await.is_err() {
            return;
        }
        if let Some(split) = split {
            if socket.write_all(&body[..split]).await.is_err() {
                return;
            }
            let _ = socket.flush().await;
            tokio::time::sleep(Duration::from_millis(15)).await;
            let _ = socket.write_all(&body[split..]).await;
        } else {
            let _ = socket.write_all(&body).await;
        }
    });
    (endpoint, rx)
}

fn messages() -> Vec<ModelMessage> {
    vec![
        ModelMessage {
            role: MessageRole::User,
            content: "Earlier question".into(),
        },
        ModelMessage {
            role: MessageRole::Assistant,
            content: "Earlier answer".into(),
        },
        ModelMessage {
            role: MessageRole::User,
            content: "Follow up".into(),
        },
    ]
}

#[tokio::test]
async fn chat_preserves_role_separated_history() {
    let body = json!({"message":{"role":"assistant","content":"Answer"},"done":true})
        .to_string()
        .into_bytes();
    let (endpoint, request) = mock(body, "200 OK", None, Duration::ZERO).await;
    let client = ModelClient::new(&endpoint, Duration::from_secs(2)).unwrap();
    assert_eq!(
        client
            .chat("test", messages(), LlmConfig::conversational())
            .await
            .unwrap(),
        "Answer"
    );
    let request = request.await.unwrap();
    assert_eq!(
        request["messages"],
        serde_json::to_value(messages()).unwrap()
    );
    assert_eq!(request["stream"], false);
}

#[tokio::test]
async fn streaming_preserves_unicode_split_between_network_chunks() {
    let content = "café 🦀";
    let body = format!(
        "{}\n{}\n",
        json!({"message":{"content":content},"done":false}),
        json!({"message":{"content":"!"},"done":true})
    )
    .into_bytes();
    let split = body.iter().position(|byte| *byte == 0xc3).unwrap() + 1;
    let (endpoint, _) = mock(body, "200 OK", Some(split), Duration::ZERO).await;
    let client = ModelClient::new(&endpoint, Duration::from_secs(2)).unwrap();
    let (tx, mut rx) = mpsc::channel(8);
    let output = client
        .chat_stream("test", messages(), LlmConfig::conversational(), tx)
        .await
        .unwrap();
    let mut streamed = String::new();
    while let Some(fragment) = rx.recv().await {
        streamed.push_str(&fragment);
    }
    assert_eq!(output, format!("{content}!"));
    assert_eq!(streamed, output);
}

#[tokio::test]
async fn truncated_stream_is_not_accepted_as_a_complete_answer() {
    let body = format!(
        "{}\n",
        json!({"message":{"content":"partial"},"done":false})
    )
    .into_bytes();
    let (endpoint, _) = mock(body, "200 OK", None, Duration::ZERO).await;
    let client = ModelClient::new(&endpoint, Duration::from_secs(2)).unwrap();
    let (tx, _rx) = mpsc::channel(8);
    assert!(matches!(
        client
            .chat_stream("test", messages(), LlmConfig::conversational(), tx)
            .await,
        Err(LlmError::InvalidResponse(_))
    ));
}

#[tokio::test]
async fn api_failures_and_request_timeouts_are_recoverable() {
    let (endpoint, _) = mock(
        br#"{"error":"model not found"}"#.to_vec(),
        "404 Not Found",
        None,
        Duration::ZERO,
    )
    .await;
    let client = ModelClient::new(&endpoint, Duration::from_secs(2)).unwrap();
    assert!(matches!(client.list_models().await, Err(LlmError::Api(_))));
    let (endpoint, _) = mock(
        br#"{"models":[]}"#.to_vec(),
        "200 OK",
        None,
        Duration::from_millis(200),
    )
    .await;
    let client = ModelClient::new(&endpoint, Duration::from_millis(20)).unwrap();
    assert!(matches!(
        client.list_models().await,
        Err(LlmError::Timeout(_))
    ));
}

#[test]
fn invalid_transport_configuration_is_rejected_without_panicking() {
    for endpoint in [
        "not a URL",
        "file:///tmp/models",
        "http://user:password@localhost",
        "http://localhost?token=secret",
    ] {
        assert!(ModelClient::new(endpoint, Duration::from_secs(1)).is_err());
    }
    assert!(ModelClient::new("http://localhost:11434", Duration::ZERO).is_err());
}
