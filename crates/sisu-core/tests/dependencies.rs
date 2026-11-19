use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sisu_core::{GeneratedPayload, parse_project_plan, persist_plan};

fn plan_json() -> Value {
    json!({
        "goal": "Build a small CLI",
        "language": "rust",
        "check_command": "cargo check",
        "milestones": [{
            "id": "M1",
            "title": "Create the CLI",
            "description": "Implement argument parsing",
            "depends_on": [],
            "done_when": ["The CLI accepts an argument"]
        }]
    })
}

#[test]
fn older_plans_default_to_no_dependencies() {
    let plan = parse_project_plan(&plan_json().to_string()).unwrap();
    let saved = serde_json::to_value(plan).unwrap();

    assert_eq!(saved["dependencies"], json!([]));
    assert_eq!(saved["milestones"][0]["depends_on"], json!([]));
}

#[test]
fn declared_dependencies_survive_disk_persistence() {
    let mut input = plan_json();
    input["dependencies"] = json!(["serde", "tokio"]);
    let plan = parse_project_plan(&input.to_string()).unwrap();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let project = std::env::temp_dir().join(format!(
        "sisu-dependency-test-{}-{unique}",
        std::process::id()
    ));

    let path = persist_plan(project.to_str().unwrap(), &plan).unwrap();
    let saved = fs::read_to_string(path).unwrap();
    fs::remove_dir_all(&project).unwrap();
    let reloaded = parse_project_plan(&saved).unwrap();

    assert_eq!(
        serde_json::to_value(reloaded).unwrap(),
        serde_json::to_value(plan).unwrap()
    );
    assert_eq!(
        serde_json::from_str::<Value>(&saved).unwrap()["dependencies"],
        json!(["serde", "tokio"])
    );
}

#[test]
fn execution_payload_can_omit_additional_dependencies() {
    let payload: GeneratedPayload = serde_json::from_value(json!({
        "files": [{"path": "main.py", "content": "print('hello')\n"}]
    }))
    .unwrap();
    let output = serde_json::to_value(payload).unwrap();

    assert_eq!(output["add_dependencies"], json!([]));
    assert_eq!(output["files"][0]["content"], "print('hello')\n");
}

#[test]
fn execution_payload_preserves_additional_package_names() {
    let input = json!({
        "files": [],
        "add_dependencies": ["zod", "@types/node"]
    });
    let payload: GeneratedPayload = serde_json::from_value(input.clone()).unwrap();

    assert_eq!(serde_json::to_value(payload).unwrap(), input);
}

#[test]
fn dependency_lists_reject_invalid_json_types() {
    for invalid in [Value::Null, json!("serde"), json!(42), json!(["serde", 42])] {
        let mut plan = plan_json();
        plan["dependencies"] = invalid.clone();
        assert!(parse_project_plan(&plan.to_string()).is_err());

        let payload = json!({"files": [], "add_dependencies": invalid});
        assert!(serde_json::from_value::<GeneratedPayload>(payload).is_err());
    }
}

#[test]
fn execution_payload_requires_files_and_rejects_command_actions() {
    for invalid in [
        json!({"add_dependencies": []}),
        json!({"files": [], "commands": ["npm install zod"]}),
        json!({"files": [{"path": "main.py", "content": "", "command": "echo hello"}]}),
    ] {
        assert!(serde_json::from_value::<GeneratedPayload>(invalid).is_err());
    }
}
