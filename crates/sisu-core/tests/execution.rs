use async_trait::async_trait;
use sisu_core::languages::get_adapter;
use sisu_core::models::ProjectPlan;
use sisu_core::sandbox::runner::SandboxRunner;
use sisu_core::workspace::snapshot;
use sisu_core::{LlmClient, Orchestrator, OrchestratorConfig, RunOutcome};
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::tempdir;

struct ScriptedLlm {
    responses: Mutex<VecDeque<String>>,
    prompts: Mutex<Vec<String>>,
}

impl ScriptedLlm {
    fn new(items: Vec<&str>) -> Self {
        Self {
            responses: Mutex::new(items.into_iter().map(String::from).collect()),
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmClient for ScriptedLlm {
    async fn generate(&self, _sys: &str, user: &str) -> Result<String, String> {
        self.prompts.lock().unwrap().push(user.to_string());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "ScriptedLlm ran out of responses".into())
    }
}

/// Uses real OS isolation if configured on this machine, otherwise falls back to
/// `unsandboxed` so compiler/test E2E suites still run on restricted Ubuntu AppArmor hosts.
fn make_runner(project_dir: &Path) -> SandboxRunner {
    let timeout = Duration::from_secs(60);
    SandboxRunner::detect(project_dir, timeout)
        .unwrap_or_else(|_| SandboxRunner::unsandboxed(timeout))
}

fn multi_milestone_plan(language: &str, dependencies: Vec<String>) -> ProjectPlan {
    serde_json::from_value(serde_json::json!({
        "goal": "Two-module math and stats library",
        "language": language,
        "check_command": "",
        "dependencies": dependencies,
        "milestones": [
            {
                "id": "M1",
                "title": "Math module",
                "description": "Implement add(a, b)",
                "depends_on": [],
                "done_when": ["add returns sum"],
                "status": "Pending",
                "attempts": 0,
                "artifacts": [],
                "last_error": null
            },
            {
                "id": "M2",
                "title": "Stats module and tests",
                "description": "Implement sum_double using M1 and verify with tests",
                "depends_on": ["M1"],
                "done_when": ["sum_double(2, 3) == 10"],
                "status": "Pending",
                "attempts": 0,
                "artifacts": [],
                "last_error": null
            }
        ]
    }))
    .expect("valid test ProjectPlan")
}

fn milestone_attempts(plan: &ProjectPlan, index: usize) -> u64 {
    serde_json::to_value(plan).unwrap()["milestones"][index]["attempts"]
        .as_u64()
        .expect("milestone attempts")
}

/// Live, 100% offline Rust E2E test:
/// M1 passes -> commits -> Tree-sitter feeds M1 signatures into M2 ->
/// M2 hits real `cargo check` E0308 -> Point D fixes type error ->
/// M2 hits real `cargo test` panic -> Point C fixes logic bug -> passes & commits.
#[tokio::test]
async fn e2e_rust_multi_milestone_real_compiler_and_tests() {
    let tmp = tempdir().unwrap();
    let project_dir = tmp.path().join("sisu_demo");
    fs::create_dir_all(&project_dir).unwrap();

    let llm = ScriptedLlm::new(vec![
        // M1: Clean math library
        r#"{"files":[
            {"path":"src/math.rs","content":"pub fn add(a: i32, b: i32) -> i32 {\n    let hidden_impl = a + b;\n    hidden_impl\n}\n"},
            {"path":"src/lib.rs","content":"pub mod math;\n"}
        ]}"#,
        // M2 initial: Deliberate Rust type error (`&str` instead of `i32` -> real E0308) + locked test
        r#"{"files":[
            {"path":"src/stats.rs","content":"use crate::math::add;\n\npub fn sum_double(a: i32, b: i32) -> i32 {\n    let wrong: i32 = \"oops\";\n    add(a, b) + wrong\n}\n"},
            {"path":"src/lib.rs","content":"pub mod math;\npub mod stats;\n"},
            {"path":"tests/stats_test.rs","content":"use sisu_demo::stats::sum_double;\n\n#[test]\nfn doubles_the_sum() {\n    assert_eq!(sum_double(2, 3), 10);\n}\n"}
        ]}"#,
        // Point D repair: Fixes type error, introduces deliberate logic bug (`* 3` instead of `* 2`)
        r#"{"patches":[{
            "path":"src/stats.rs",
            "original_snippet":"pub fn sum_double(a: i32, b: i32) -> i32 {\n    let wrong: i32 = \"oops\";\n    add(a, b) + wrong\n}",
            "replacement_snippet":"pub fn sum_double(a: i32, b: i32) -> i32 {\n    add(a, b) * 3\n}"
        }]}"#,
        // Point C repair: Fixes assertion failure caught by real `cargo test`
        r#"{"patches":[{
            "path":"src/stats.rs",
            "original_snippet":"pub fn sum_double(a: i32, b: i32) -> i32 {\n    add(a, b) * 3\n}",
            "replacement_snippet":"pub fn sum_double(a: i32, b: i32) -> i32 {\n    add(a, b) * 2\n}"
        }]}"#,
    ]);

    let adapter = get_adapter("rust").unwrap();
    let runner = make_runner(&project_dir);
    let orch = Orchestrator::new(
        &llm,
        adapter.as_ref(),
        &runner,
        OrchestratorConfig::default(),
    );

    let mut plan = multi_milestone_plan("rust", vec![]);
    let outcome = orch.run_project(&project_dir, &mut plan).await.unwrap();

    assert!(
        matches!(outcome, RunOutcome::CompletedAll { completed_count: 2 }),
        "Expected CompletedAll, got {outcome:?}"
    );
    assert_eq!(milestone_attempts(&plan, 0), 1);
    assert_eq!(milestone_attempts(&plan, 1), 3); // initial + Point D (E0308) + Point C (cargo test)

    let prompts = llm.prompts();
    // M2 generation prompt received M1's AST signature without implementation body
    assert!(prompts[1].contains("pub fn add(a: i32, b: i32) -> i32 { ... }"));
    assert!(!prompts[1].contains("hidden_impl"));
    // Point D prompt received the real rustc E0308 diagnostic and enclosing function
    assert!(prompts[2].contains("E0308"));
    assert!(prompts[2].contains("pub fn sum_double"));
    // Point C prompt received the real `cargo test` failure
    assert!(prompts[3].contains("cargo_test_failure"));

    assert!(!snapshot::current_head(&project_dir).unwrap().is_empty());
}

/// Live Python E2E test (`uv` + `pyright` + `pytest`).
/// Run manually with: `cargo test -p sisu-core --test execution e2e_python -- --ignored --nocapture`
#[tokio::test]
#[ignore = "requires uv, pyright, and network access to install pytest"]
async fn e2e_python_multi_milestone_real_toolchain() {
    let dir = tempdir().unwrap();

    let llm = ScriptedLlm::new(vec![
        // M1: Clean Python module
        r#"{"files":[
            {"path":"pkg/__init__.py","content":""},
            {"path":"pkg/math_utils.py","content":"def add(a: int, b: int) -> int:\n    return a + b\n"}
        ]}"#,
        // M2: Deliberate logic bug caught by real `pytest`
        r#"{"files":[
            {"path":"pkg/stats.py","content":"from pkg.math_utils import add\n\ndef sum_double(a: int, b: int) -> int:\n    return add(a, b) * 3\n"},
            {"path":"tests/test_stats.py","content":"from pkg.stats import sum_double\n\ndef test_sum_double() -> None:\n    assert sum_double(2, 3) == 10\n"}
        ]}"#,
        // Point D surgical repair
        r#"{"patches":[{
            "path":"pkg/stats.py",
            "original_snippet":"def sum_double(a: int, b: int) -> int:\n    return add(a, b) * 3",
            "replacement_snippet":"def sum_double(a: int, b: int) -> int:\n    return add(a, b) * 2"
        }]}"#,
    ]);

    let adapter = get_adapter("python").unwrap();
    let runner = make_runner(dir.path());
    let orch = Orchestrator::new(
        &llm,
        adapter.as_ref(),
        &runner,
        OrchestratorConfig::default(),
    );

    let mut plan = multi_milestone_plan("python", vec!["pytest".into()]);
    let outcome = orch.run_project(dir.path(), &mut plan).await.unwrap();

    assert!(
        matches!(outcome, RunOutcome::CompletedAll { completed_count: 2 }),
        "Expected CompletedAll, got {outcome:?}"
    );
    assert_eq!(milestone_attempts(&plan, 1), 2);
}

/// Live TypeScript E2E test (`npm` + `tsc` + `vitest`).
/// Run manually with: `cargo test -p sisu-core --test execution e2e_typescript -- --ignored --nocapture`
#[tokio::test]
#[ignore = "requires node, npm, and network access to install typescript + vitest"]
async fn e2e_typescript_multi_milestone_real_toolchain() {
    let dir = tempdir().unwrap();

    let llm = ScriptedLlm::new(vec![
        // M1: Clean TypeScript module
        r#"{"files":[
            {"path":"src/math.ts","content":"export function add(a: number, b: number): number {\n    return a + b;\n}\n"}
        ]}"#,
        // M2: Deliberate TS2322 type error caught by real `tsc --noEmit`
        r#"{"files":[
            {"path":"src/stats.ts","content":"import { add } from \"./math.js\";\n\nexport function sumDouble(a: number, b: number): number {\n    const bad: number = \"wrong\";\n    return add(a, b) * 2 + bad;\n}\n"},
            {"path":"tests/stats.test.ts","content":"import { describe, expect, it } from \"vitest\";\nimport { sumDouble } from \"../src/stats.js\";\n\ndescribe(\"sumDouble\", () => {\n    it(\"doubles the sum\", () => {\n        expect(sumDouble(2, 3)).toBe(10);\n    });\n});\n"}
        ]}"#,
        // Point D surgical repair
        r#"{"patches":[{
            "path":"src/stats.ts",
            "original_snippet":"export function sumDouble(a: number, b: number): number {\n    const bad: number = \"wrong\";\n    return add(a, b) * 2 + bad;\n}",
            "replacement_snippet":"export function sumDouble(a: number, b: number): number {\n    return add(a, b) * 2;\n}"
        }]}"#,
    ]);

    let adapter = get_adapter("typescript").unwrap();
    let runner = make_runner(dir.path());
    let orch = Orchestrator::new(
        &llm,
        adapter.as_ref(),
        &runner,
        OrchestratorConfig::default(),
    );

    let mut plan = multi_milestone_plan("typescript", vec![]);
    let outcome = orch.run_project(dir.path(), &mut plan).await.unwrap();

    assert!(
        matches!(outcome, RunOutcome::CompletedAll { completed_count: 2 }),
        "Expected CompletedAll, got {outcome:?}"
    );
    assert_eq!(milestone_attempts(&plan, 1), 2);
}
