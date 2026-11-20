use async_trait::async_trait;
use sisu_core::languages::LanguageAdapter;
use sisu_core::models::{Diagnostic, ProjectPlan, TaskStatus};
use sisu_core::sandbox::runner::SandboxRunner;
use sisu_core::workspace::snapshot;
use sisu_core::{LlmClient, Orchestrator, OrchestratorConfig, RunOutcome, load_plan};
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::tempdir;

struct RecordingMockLlm {
    responses: Mutex<VecDeque<String>>,
    prompts_seen: Mutex<Vec<String>>,
}

impl RecordingMockLlm {
    fn new(items: Vec<&str>) -> Self {
        Self {
            responses: Mutex::new(items.into_iter().map(String::from).collect()),
            prompts_seen: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        self.prompts_seen.lock().unwrap().len()
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts_seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmClient for RecordingMockLlm {
    async fn generate(&self, _sys: &str, user: &str) -> Result<String, String> {
        self.prompts_seen.lock().unwrap().push(user.to_string());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "RecordingMockLlm ran out of scripted responses".into())
    }
}

struct ScriptedAdapter {
    static_queue: Mutex<VecDeque<Vec<Diagnostic>>>,
    test_queue: Mutex<VecDeque<Vec<Diagnostic>>>,
}

impl ScriptedAdapter {
    fn new(static_results: Vec<Vec<Diagnostic>>, test_results: Vec<Vec<Diagnostic>>) -> Self {
        Self {
            static_queue: Mutex::new(static_results.into()),
            test_queue: Mutex::new(test_results.into()),
        }
    }
}

#[async_trait]
impl LanguageAdapter for ScriptedAdapter {
    fn name(&self) -> &'static str {
        "python"
    }

    async fn scaffold(&self, _dir: &Path, _runner: &SandboxRunner) -> Result<(), String> {
        Ok(())
    }

    async fn install_deps(
        &self,
        _dir: &Path,
        _deps: &[String],
        _runner: &SandboxRunner,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn check_static(
        &self,
        _dir: &Path,
        _runner: &SandboxRunner,
    ) -> Result<Vec<Diagnostic>, String> {
        Ok(self
            .static_queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default())
    }

    async fn run_tests(
        &self,
        _dir: &Path,
        _runner: &SandboxRunner,
    ) -> Result<Vec<Diagnostic>, String> {
        Ok(self
            .test_queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default())
    }
}

fn two_milestone_plan() -> ProjectPlan {
    serde_json::from_str(
        r#"{
            "goal":"Math and stats package",
            "language":"python",
            "check_command":"",
            "dependencies":[],
            "milestones":[
                {
                    "id":"M1",
                    "title":"Core math",
                    "description":"Implement add(a, b)",
                    "depends_on":[],
                    "done_when":["add works"],
                    "status":"Pending",
                    "attempts":0,
                    "artifacts":[],
                    "last_error":null
                },
                {
                    "id":"M2",
                    "title":"Stats helper",
                    "description":"Implement double_sum using M1",
                    "depends_on":["M1"],
                    "done_when":["double_sum works"],
                    "status":"Pending",
                    "attempts":0,
                    "artifacts":[],
                    "last_error":null
                }
            ]
        }"#,
    )
    .unwrap()
}

fn make_diag(file: &str, line: usize, code: &str, msg: &str) -> Diagnostic {
    Diagnostic {
        file: file.into(),
        line,
        column: 1,
        code: Some(code.into()),
        message: msg.into(),
        related_spans: vec![],
        local_hint: None,
    }
}

#[tokio::test]
async fn resumes_from_milestones_json_after_m2_failure_and_git_rollback() {
    let dir = tempdir().unwrap();
    let runner = SandboxRunner::unsandboxed(Duration::from_secs(5));

    // --- RUN 1: M1 passes & commits; M2 fails via oscillation ---
    let llm_run1 = RecordingMockLlm::new(vec![
        // M1 generation
        r#"{"files":[{"path":"src/math.py","content":"def add(a: int, b: int) -> int:\n    secret_body = a + b\n    return secret_body\n"}]}"#,
        // M2 generation (broken)
        r#"{"files":[{"path":"src/stats.py","content":"def double_sum(a: int, b: int) -> int:\n    return 'broken'\n"}]}"#,
        // M2 Point D patch still produces identical type error -> oscillation -> Point A
        r#"{"patches":[{"path":"src/stats.py","original_snippet":"    return 'broken'","replacement_snippet":"    return 'still_broken'"}]}"#,
    ]);
    let persistent_err = vec![make_diag(
        "src/stats.py",
        2,
        "reportReturnType",
        "str is not assignable to int",
    )];
    let adapter_run1 = ScriptedAdapter::new(
        vec![vec![], persistent_err.clone(), persistent_err],
        vec![vec![]],
    );

    let mut plan = two_milestone_plan();
    let orch1 = Orchestrator::new(
        &llm_run1,
        &adapter_run1,
        &runner,
        OrchestratorConfig {
            auto_rollback_on_failure: false,
        },
    );

    let outcome1 = orch1.run_project(dir.path(), &mut plan).await.unwrap();
    assert!(matches!(outcome1, RunOutcome::FailedAtMilestone(ref r) if r.milestone_id == "M2"));

    // Simulate interactive user confirming rollback at Point A
    snapshot::rollback_hard(dir.path()).unwrap();
    assert!(dir.path().join("src/math.py").exists()); // M1 survived rollback
    assert!(!dir.path().join("src/stats.py").exists()); // Broken M2 wiped
    assert!(dir.path().join("milestones.json").exists()); // State file survived

    // --- RUN 2: Load state from disk and resume ---
    let mut resumed_plan = load_plan(dir.path())
        .unwrap()
        .expect("milestones.json should exist");
    let resumed_state = serde_json::to_value(&resumed_plan).unwrap();
    assert_eq!(
        resumed_state["milestones"][0]["status"],
        serde_json::to_value(TaskStatus::Completed).unwrap()
    );
    assert!(
        serde_json::to_string(&resumed_state["milestones"][1]["status"])
            .unwrap()
            .contains("Failed")
    );

    let llm_run2 = RecordingMockLlm::new(vec![
        // Only M2 should be requested; M1 must be skipped
        r#"{"files":[{"path":"src/stats.py","content":"from src.math import add\n\ndef double_sum(a: int, b: int) -> int:\n    return add(a, b) * 2\n"}]}"#,
    ]);
    let adapter_run2 = ScriptedAdapter::new(vec![vec![]], vec![vec![]]);
    let orch2 = Orchestrator::new(
        &llm_run2,
        &adapter_run2,
        &runner,
        OrchestratorConfig::default(),
    );

    let outcome2 = orch2
        .run_project(dir.path(), &mut resumed_plan)
        .await
        .unwrap();
    assert!(matches!(
        outcome2,
        RunOutcome::CompletedAll { completed_count: 2 }
    ));

    // Verify M1 was skipped and its AST signature (without body) was passed into M2's prompt
    assert_eq!(llm_run2.call_count(), 1);
    let m2_prompt = &llm_run2.prompts()[0];
    assert!(m2_prompt.contains("def add(a: int, b: int) -> int: ..."));
    assert!(!m2_prompt.contains("secret_body"));
}

#[tokio::test]
async fn exhausts_d_c_b_auto_rolls_back_tracked_and_untracked_and_keeps_report() {
    let dir = tempdir().unwrap();
    let runner = SandboxRunner::unsandboxed(Duration::from_secs(5));

    let llm = RecordingMockLlm::new(vec![
        // M1 clean
        r#"{"files":[{"path":"src/math.py","content":"def add(a: int, b: int) -> int:\n    return a + b\n"}]}"#,
        // M2 corrupts M1's tracked file AND creates an untracked file
        r#"{"files":[{"path":"src/math.py","content":"CORRUPTED_BY_M2\n"},{"path":"src/extra.py","content":"x = 1\n"}]}"#,
        // D, C, B each produce distinct errors so all 3 levels run to exhaustion
        r#"{"patches":[{"path":"src/extra.py","original_snippet":"x = 1","replacement_snippet":"x = 2"}]}"#,
        r#"{"patches":[{"path":"src/extra.py","original_snippet":"x = 2","replacement_snippet":"x = 3"}]}"#,
        r#"{"patches":[{"path":"src/extra.py","original_snippet":"x = 3","replacement_snippet":"x = 4"}]}"#,
    ]);
    let adapter = ScriptedAdapter::new(
        vec![
            vec![], // M1 passes
            vec![make_diag("src/extra.py", 1, "E1", "initial error")],
            vec![make_diag("src/extra.py", 1, "E2", "error after D")],
            vec![make_diag("src/extra.py", 1, "E3", "error after C")],
            vec![make_diag("src/extra.py", 1, "E4", "error after B")],
        ],
        vec![vec![]],
    );

    let mut plan = two_milestone_plan();
    let orch = Orchestrator::new(
        &llm,
        &adapter,
        &runner,
        OrchestratorConfig {
            auto_rollback_on_failure: true,
        },
    );

    let outcome = orch.run_project(dir.path(), &mut plan).await.unwrap();
    match outcome {
        RunOutcome::FailedAtMilestone(report) => {
            assert_eq!(report.milestone_id, "M2");
            assert!(report.rolled_back);
            assert!(report.reason.contains("D -> C -> B -> A"));
            // Report captured final broken content BEFORE rollback deleted `src/extra.py`
            assert!(report.copy_paste_prompt.contains("x = 4"));
            assert!(report.copy_paste_prompt.contains("E4"));
        }
        other => panic!("Expected FailedAtMilestone, got {other:?}"),
    }

    // Tracked M1 file restored cleanly; untracked M2 file deleted; M2 spent 4 attempts (gen + D + C + B)
    let math_content = fs::read_to_string(dir.path().join("src/math.py")).unwrap();
    assert!(math_content.contains("return a + b"));
    assert!(!math_content.contains("CORRUPTED_BY_M2"));
    assert!(!dir.path().join("src/extra.py").exists());
    let plan_state = serde_json::to_value(&plan).unwrap();
    assert_eq!(plan_state["milestones"][1]["attempts"], 4);
}

#[tokio::test]
async fn blocks_repair_from_tampering_with_locked_test_file() {
    let dir = tempdir().unwrap();
    let runner = SandboxRunner::unsandboxed(Duration::from_secs(5));

    let llm = RecordingMockLlm::new(vec![
        // M1 writes buggy code + locked test
        r#"{"files":[
            {"path":"src/calc.py","content":"def add(a: int, b: int) -> int:\n    return a - b\n"},
            {"path":"tests/test_calc.py","content":"from src.calc import add\nassert add(2, 3) == 5\n"}
        ]}"#,
        // Point D: lazy model tries to weaken the test file -> MUST be rejected
        r#"{"patches":[{"path":"tests/test_calc.py","original_snippet":"assert add(2, 3) == 5","replacement_snippet":"assert add(2, 3) == -1"}]}"#,
        // Point C: model fixes actual implementation instead
        r#"{"patches":[{"path":"src/calc.py","original_snippet":"def add(a: int, b: int) -> int:\n    return a - b","replacement_snippet":"def add(a: int, b: int) -> int:\n    return a + b"}]}"#,
    ]);

    let test_fail = vec![make_diag(
        "tests/test_calc.py",
        2,
        "pytest_failure",
        "assert -1 == 5",
    )];
    // Gate 1 passes twice; Gate 2 fails initially, then passes after Point C fixes src/calc.py
    let adapter = ScriptedAdapter::new(vec![vec![], vec![]], vec![test_fail, vec![]]);

    let mut plan: ProjectPlan = serde_json::from_str(
        r#"{
            "goal":"Calculator",
            "language":"python",
            "check_command":"",
            "dependencies":[],
            "milestones":[{
                "id":"M1",
                "title":"Add",
                "description":"Implement add",
                "depends_on":[],
                "done_when":["add(2, 3) == 5"],
                "status":"Pending",
                "attempts":0,
                "artifacts":[],
                "last_error":null
            }]
        }"#,
    )
    .unwrap();

    let orch = Orchestrator::new(&llm, &adapter, &runner, OrchestratorConfig::default());
    let outcome = orch.run_project(dir.path(), &mut plan).await.unwrap();

    assert!(matches!(
        outcome,
        RunOutcome::CompletedAll { completed_count: 1 }
    ));
    let test_src = fs::read_to_string(dir.path().join("tests/test_calc.py")).unwrap();
    assert!(
        test_src.contains("== 5"),
        "Locked test file must remain untouched"
    );
    let impl_src = fs::read_to_string(dir.path().join("src/calc.py")).unwrap();
    assert!(impl_src.contains("return a + b"));
}
