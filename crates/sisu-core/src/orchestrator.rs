use crate::languages::LanguageAdapter;
use crate::llm::prompts::{EXECUTOR_SYSTEM_PROMPT, PROJECT_SYSTEM_PROMPT, REPAIR_SYSTEM_PROMPT};
use crate::models::{
    Diagnostic, EscalationLevel, GeneratedPayload, Milestone, ProjectPlan, RepairPayload,
    TaskStatus,
};
use crate::sandbox::runner::SandboxRunner;
use crate::workspace::{context, files, snapshot};
use async_trait::async_trait;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Decouples the orchestrator from Ollama so both production and deterministic tests work.
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn generate(&self, system_prompt: &str, user_prompt: &str) -> Result<String, String>;
}

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// If true, automatically runs `git reset --hard HEAD` + `git clean -fd` on Point A.
    /// Set to false in interactive CLI mode if you want to ask the user first.
    pub auto_rollback_on_failure: bool,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            auto_rollback_on_failure: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FrontierDebugReport {
    pub milestone_id: String,
    pub reason: String,
    pub diagnostics: Vec<Diagnostic>,
    /// Ready-to-copy markdown prompt for a frontier model (Point A bonus).
    pub copy_paste_prompt: String,
    pub rolled_back: bool,
}

#[derive(Debug, Clone)]
pub enum RunOutcome {
    CompletedAll { completed_count: usize },
    FailedAtMilestone(FrontierDebugReport),
}

/// Generates a new `ProjectPlan` via LLM Job #0 (Planner).
pub async fn create_plan(
    llm: &dyn LlmClient,
    goal: &str,
    language: &str,
) -> Result<ProjectPlan, String> {
    let user_prompt = format!(
        "Project Goal: {goal}\n\
         Target Language: {language}\n\
         Produce a minimal, dependency-ordered milestone plan in valid JSON."
    );

    let raw = llm.generate(PROJECT_SYSTEM_PROMPT, &user_prompt).await?;
    let mut plan: ProjectPlan = parse_json_payload(&raw)?;

    if plan.goal.trim().is_empty() {
        plan.goal = goal.to_string();
    }
    if plan.language.trim().is_empty() {
        plan.language = language.to_string();
    }

    Ok(plan)
}

/// Loads an existing `milestones.json` to resume after a crash or failure, if present.
pub fn load_plan(project_dir: &Path) -> Result<Option<ProjectPlan>, String> {
    let path = project_dir.join("milestones.json");
    if !path.exists() {
        return Ok(None);
    }

    let raw =
        fs::read_to_string(&path).map_err(|e| format!("Failed to read milestones.json: {e}"))?;
    let plan: ProjectPlan =
        serde_json::from_str(&raw).map_err(|e| format!("Failed to parse milestones.json: {e}"))?;
    Ok(Some(plan))
}

pub struct Orchestrator<'a> {
    llm: &'a dyn LlmClient,
    adapter: &'a dyn LanguageAdapter,
    runner: &'a SandboxRunner,
    config: OrchestratorConfig,
}

impl<'a> Orchestrator<'a> {
    pub fn new(
        llm: &'a dyn LlmClient,
        adapter: &'a dyn LanguageAdapter,
        runner: &'a SandboxRunner,
        config: OrchestratorConfig,
    ) -> Self {
        Self {
            llm,
            adapter,
            runner,
            config,
        }
    }

    /// Runs Pre-Flight once, then executes every non-completed milestone linearly.
    pub async fn run_project(
        &self,
        project_dir: &Path,
        plan: &mut ProjectPlan,
    ) -> Result<RunOutcome, String> {
        fs::create_dir_all(project_dir)
            .map_err(|e| format!("Failed to create project_dir: {e}"))?;

        // --- STEP 1: PRE-FLIGHT (Runs once before M1) ---
        self.adapter.scaffold(project_dir, self.runner).await?;
        self.adapter
            .install_deps(project_dir, &plan.dependencies, self.runner)
            .await?;
        snapshot::init_repo(project_dir)?;
        save_plan_state(project_dir, plan)?;

        // --- STEP 2: LINEAR MILESTONE LOOP ---
        for idx in 0..plan.milestones.len() {
            if plan.milestones[idx].status == TaskStatus::Completed {
                continue;
            }

            plan.milestones[idx].status = TaskStatus::InProgress;
            plan.milestones[idx].last_error = None;
            save_plan_state(project_dir, plan)?;

            let milestone_snapshot = plan.milestones[idx].clone();
            let depends_on_artifacts = collect_dependency_artifacts(plan, &milestone_snapshot);

            match self
                .execute_milestone(
                    project_dir,
                    &plan.goal,
                    &plan.language,
                    &milestone_snapshot,
                    &depends_on_artifacts,
                )
                .await?
            {
                MilestoneResult::Passed {
                    artifacts,
                    attempts,
                } => {
                    let commit_msg = format!(
                        "sisu: complete {} - {}",
                        milestone_snapshot.id, milestone_snapshot.title
                    );
                    snapshot::commit_checkpoint(project_dir, &commit_msg)?;

                    let m = &mut plan.milestones[idx];
                    m.artifacts = artifacts;
                    m.attempts = attempts;
                    m.status = TaskStatus::Completed;
                    m.last_error = None;
                    save_plan_state(project_dir, plan)?;
                }
                MilestoneResult::Failed {
                    mut report,
                    attempts,
                } => {
                    if self.config.auto_rollback_on_failure {
                        snapshot::rollback_hard(project_dir)?;
                        report.rolled_back = true;
                    }

                    let m = &mut plan.milestones[idx];
                    m.attempts = attempts;
                    m.status = TaskStatus::Failed(report.reason.clone());
                    m.last_error = Some(report.reason.clone());
                    save_plan_state(project_dir, plan)?;

                    return Ok(RunOutcome::FailedAtMilestone(report));
                }
            }
        }

        let completed_count = plan
            .milestones
            .iter()
            .filter(|m| m.status == TaskStatus::Completed)
            .count();

        Ok(RunOutcome::CompletedAll { completed_count })
    }

    async fn execute_milestone(
        &self,
        project_dir: &Path,
        goal: &str,
        language: &str,
        milestone: &Milestone,
        depends_on_artifacts: &[String],
    ) -> Result<MilestoneResult, String> {
        // 1. Generate code (LLM Job #1)
        let gen_prompt = context::build_generation_prompt(
            project_dir,
            goal,
            language,
            milestone,
            depends_on_artifacts,
        );
        let raw = self
            .llm
            .generate(EXECUTOR_SYSTEM_PROMPT, &gen_prompt)
            .await?;
        let payload: GeneratedPayload = parse_json_payload(&raw)?;

        // 2. Install any newly declared dependencies (Network ON)
        if !payload.add_dependencies.is_empty() {
            self.adapter
                .install_deps(project_dir, &payload.add_dependencies, self.runner)
                .await?;
        }

        // 3. Write files through the path jail
        let written_artifacts = files::write_generated_files(project_dir, &payload.files)?;
        let locked_tests: Vec<String> = written_artifacts
            .iter()
            .filter(|p| is_test_file(p))
            .cloned()
            .collect();

        // 4. Two-Gate Verification
        let mut diagnostics = self.verify_two_gates(project_dir).await?;
        if diagnostics.is_empty() {
            return Ok(MilestoneResult::Passed {
                artifacts: written_artifacts,
                attempts: 1,
            });
        }

        // 5. Escalating Repair Loop (Missing Dep Auto-Install + D -> C -> B -> A)
        let mut installed_auto_deps: HashSet<String> = HashSet::new();
        let mut seen_fingerprints: HashSet<String> = HashSet::new();
        let mut attempts: u8 = 1;

        // Try auto-installing missing packages (max 1 attempt per package name)
        while let Some(pkg) = find_missing_package(&diagnostics) {
            if !installed_auto_deps.insert(pkg.clone()) {
                break; // Already tried installing this package once; stop looping
            }

            if self
                .adapter
                .install_deps(project_dir, &[pkg], self.runner)
                .await
                .is_err()
            {
                break; // Fake/invalid package -> drop into D -> C -> B repair loop
            }

            diagnostics = self.verify_two_gates(project_dir).await?;
            if diagnostics.is_empty() {
                return Ok(MilestoneResult::Passed {
                    artifacts: written_artifacts,
                    attempts,
                });
            }
        }

        seen_fingerprints.insert(diagnostic_fingerprint(&diagnostics));

        for level in [EscalationLevel::D, EscalationLevel::C, EscalationLevel::B] {
            attempts = attempts.saturating_add(1);
            // 1. Fill `diag.local_hint` with real library stubs from `.venv` / disk
            context::enrich_diagnostics_with_stubs(
                project_dir,
                language,
                self.adapter,
                &mut diagnostics,
            );

            // 2. Call the new AST-powered prompt builder in `workspace/context.rs`
            let repair_prompt = context::build_repair_prompt(
                project_dir,
                language,
                milestone,
                level,
                &diagnostics,
                depends_on_artifacts,
            );

            let raw_repair = self
                .llm
                .generate(REPAIR_SYSTEM_PROMPT, &repair_prompt)
                .await?;

            let repair_payload: RepairPayload = match parse_json_payload(&raw_repair) {
                Ok(p) => p,
                Err(parse_err) => {
                    // Bad JSON counts as a failed level attempt; escalate cleanly
                    diagnostics.push(Diagnostic {
                        file: String::new(),
                        line: 0,
                        column: 0,
                        code: Some("repair_json_error".into()),
                        message: parse_err,
                        related_spans: vec![],
                        local_hint: None,
                    });
                    continue;
                }
            };

            if let Err(patch_err) =
                files::apply_patches(project_dir, &repair_payload.patches, &locked_tests)
            {
                diagnostics.push(Diagnostic {
                    file: String::new(),
                    line: 0,
                    column: 0,
                    code: Some("patch_rejected".into()),
                    message: patch_err,
                    related_spans: vec![],
                    local_hint: None,
                });
                continue;
            }

            diagnostics = self.verify_two_gates(project_dir).await?;
            if diagnostics.is_empty() {
                return Ok(MilestoneResult::Passed {
                    artifacts: written_artifacts,
                    attempts,
                });
            }

            // Oscillation check: if we hit an identical error state we've already seen, abort to Point A
            let fp = diagnostic_fingerprint(&diagnostics);
            if !seen_fingerprints.insert(fp) {
                let reason = format!(
                    "Error oscillation detected after escalation {:?} (repeated diagnostic)",
                    level
                );
                let report = build_frontier_report(
                    project_dir,
                    goal,
                    language,
                    milestone,
                    &reason,
                    &diagnostics,
                    attempts,
                );
                return Ok(MilestoneResult::Failed { report, attempts });
            }
        }

        // --- POINT A: Exhausted D -> C -> B ---
        let reason = format!(
            "Escalation exhausted (D -> C -> B -> A) with {} remaining diagnostic(s)",
            diagnostics.len()
        );
        let report = build_frontier_report(
            project_dir,
            goal,
            language,
            milestone,
            &reason,
            &diagnostics,
            attempts,
        );

        Ok(MilestoneResult::Failed { report, attempts })
    }

    /// Gate 1 (Static) -> Gate 2 (Behavioral Tests). Stops immediately if Gate 1 fails.
    async fn verify_two_gates(&self, project_dir: &Path) -> Result<Vec<Diagnostic>, String> {
        let static_diags = self.adapter.check_static(project_dir, self.runner).await?;
        if !static_diags.is_empty() {
            return Ok(static_diags);
        }

        self.adapter.run_tests(project_dir, self.runner).await
    }
}

enum MilestoneResult {
    Passed {
        artifacts: Vec<String>,
        attempts: u8,
    },
    Failed {
        report: FrontierDebugReport,
        attempts: u8,
    },
}

// --- Pure Deterministic Helpers ---

fn save_plan_state(project_dir: &Path, plan: &ProjectPlan) -> Result<(), String> {
    let json = serde_json::to_string_pretty(plan)
        .map_err(|e| format!("Failed to serialize milestones.json: {e}"))?;
    fs::write(project_dir.join("milestones.json"), json)
        .map_err(|e| format!("Failed to write milestones.json: {e}"))
}

fn collect_dependency_artifacts(plan: &ProjectPlan, milestone: &Milestone) -> Vec<String> {
    let mut paths = Vec::new();
    for dep_id in &milestone.depends_on {
        if let Some(prev) = plan.milestones.iter().find(|m| &m.id == dep_id) {
            for art in &prev.artifacts {
                if !paths.contains(art) {
                    paths.push(art.clone());
                }
            }
        }
    }
    paths
}

fn is_test_file(rel_path: &str) -> bool {
    let normalized = rel_path.replace('\\', "/");
    let file_name = Path::new(&normalized)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    normalized.starts_with("tests/")
        || normalized.contains("/tests/")
        || file_name.starts_with("test_")
        || file_name.ends_with("_test.py")
        || file_name.ends_with("_test.rs")
        || file_name.ends_with(".test.ts")
        || file_name.ends_with(".spec.ts")
}

/// Strips accidental markdown fences (` ```json ... ``` `) emitted by local 7B models.
pub fn strip_json_fences(raw: &str) -> &str {
    let trimmed = raw.trim();
    let without_open = if let Some(rest) = trimmed.strip_prefix("```") {
        // Drop optional language tag on the first line (e.g. "json\n")
        match rest.find('\n') {
            Some(idx) => &rest[idx + 1..],
            None => rest,
        }
    } else {
        trimmed
    };

    without_open
        .trim_end()
        .strip_suffix("```")
        .unwrap_or(without_open)
        .trim()
}

fn parse_json_payload<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, String> {
    let cleaned = strip_json_fences(raw);
    serde_json::from_str(cleaned).map_err(|e| {
        format!(
            "Invalid LLM JSON payload: {e}\nRaw snippet: {:.200}",
            cleaned
        )
    })
}

fn diagnostic_fingerprint(diagnostics: &[Diagnostic]) -> String {
    let mut keys: Vec<String> = diagnostics
        .iter()
        .map(|d| {
            format!(
                "{}:{}:{}:{}",
                d.file,
                d.line,
                d.code.as_deref().unwrap_or(""),
                d.message.trim()
            )
        })
        .collect();
    keys.sort();
    keys.join("|")
}

/// Extracts a top-level missing package name from Pyright or Python runtime diagnostics.
pub fn find_missing_package(diagnostics: &[Diagnostic]) -> Option<String> {
    for d in diagnostics {
        // Pyright: `Import "requests" could not be resolved`
        if let Some(rest) = d.message.strip_prefix("Import \"") {
            if let Some(pkg) = rest.split('"').next() {
                if let Some(top) = pkg.split('.').next() {
                    if !top.is_empty() {
                        return Some(top.to_string());
                    }
                }
            }
        }
        // Pytest / Python runtime: `No module named 'pydantic'`
        if let Some(idx) = d.message.find("No module named '") {
            let rest = &d.message[idx + "No module named '".len()..];
            if let Some(pkg) = rest.split('\'').next() {
                if let Some(top) = pkg.split('.').next() {
                    if !top.is_empty() {
                        return Some(top.to_string());
                    }
                }
            }
        }
    }
    None
}

fn build_frontier_report(
    project_dir: &Path,
    goal: &str,
    language: &str,
    milestone: &Milestone,
    reason: &str,
    diagnostics: &[Diagnostic],
    attempts: u8,
) -> FrontierDebugReport {
    let mut md = format!(
        "# SISU Debug Handoff — Milestone {}\n\n\
         **Project Goal:** {goal}\n\
         **Language:** {language}\n\
         **Milestone:** {} — {}\n\
         **Failure Reason:** {reason} (after {attempts} attempts)\n\n\
         ## Diagnostics\n",
        milestone.id, milestone.id, milestone.title
    );

    for d in diagnostics {
        md.push_str(&format!(
            "- `{}:{}` (`{}`): {}\n",
            d.file,
            d.line,
            d.code.as_deref().unwrap_or("error"),
            d.message
        ));
    }

    if let Some(primary) = diagnostics.iter().find(|d| !d.file.is_empty()) {
        if let Ok(content) = fs::read_to_string(project_dir.join(&primary.file)) {
            md.push_str(&format!(
                "\n## Failing File (`{}`)\n```{}\n{}\n```\n",
                primary.file, language, content
            ));
        }
    }

    md.push_str("\n## Request\nPlease identify the root cause and provide the minimal diff to fix this error.\n");

    FrontierDebugReport {
        milestone_id: milestone.id.clone(),
        reason: reason.to_string(),
        diagnostics: diagnostics.to_vec(),
        copy_paste_prompt: md,
        rolled_back: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;
    use tempfile::tempdir;

    struct MockLlm {
        responses: Mutex<VecDeque<String>>,
    }

    impl MockLlm {
        fn new(items: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(items.into_iter().map(String::from).collect()),
            }
        }
    }

    #[async_trait]
    impl LlmClient for MockLlm {
        async fn generate(&self, _sys: &str, _user: &str) -> Result<String, String> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "MockLlm ran out of scripted responses".into())
        }
    }

    struct MockAdapter {
        static_queue: Mutex<VecDeque<Vec<Diagnostic>>>,
        test_queue: Mutex<VecDeque<Vec<Diagnostic>>>,
        installed: Mutex<Vec<Vec<String>>>,
    }

    impl MockAdapter {
        fn new(static_results: Vec<Vec<Diagnostic>>, test_results: Vec<Vec<Diagnostic>>) -> Self {
            Self {
                static_queue: Mutex::new(static_results.into()),
                test_queue: Mutex::new(test_results.into()),
                installed: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LanguageAdapter for MockAdapter {
        fn name(&self) -> &'static str {
            "python"
        }

        async fn scaffold(&self, _dir: &Path, _runner: &SandboxRunner) -> Result<(), String> {
            Ok(())
        }

        async fn install_deps(
            &self,
            _dir: &Path,
            deps: &[String],
            _runner: &SandboxRunner,
        ) -> Result<(), String> {
            if !deps.is_empty() {
                self.installed.lock().unwrap().push(deps.to_vec());
            }
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

    fn sample_plan() -> ProjectPlan {
        ProjectPlan {
            goal: "Build a math utility".into(),
            language: "python".into(),
            check_command: Default::default(),
            dependencies: vec![],
            milestones: vec![Milestone {
                id: "M1".into(),
                title: "Add function".into(),
                description: "Implement add(a, b)".into(),
                depends_on: vec![],
                done_when: vec!["add(2, 3) == 5".into()],
                status: TaskStatus::Pending,
                attempts: 0,
                artifacts: vec![],
                last_error: None,
            }],
        }
    }

    #[tokio::test]
    async fn happy_path_writes_verifies_and_commits() {
        let dir = tempdir().unwrap();
        let llm = MockLlm::new(vec![
            r#"```json
            {"files":[{"path":"src/math.py","content":"def add(a, b):\n    return a + b\n"}],"add_dependencies":[]}
            ```"#,
        ]);
        let adapter = MockAdapter::new(vec![vec![]], vec![vec![]]);
        let runner = SandboxRunner::unsandboxed(Duration::from_secs(5));
        let orch = Orchestrator::new(&llm, &adapter, &runner, OrchestratorConfig::default());

        let mut plan = sample_plan();
        let outcome = orch.run_project(dir.path(), &mut plan).await.unwrap();

        assert!(matches!(
            outcome,
            RunOutcome::CompletedAll { completed_count: 1 }
        ));
        assert_eq!(plan.milestones[0].status, TaskStatus::Completed);
        assert_eq!(plan.milestones[0].attempts, 1);
        assert!(dir.path().join("src/math.py").exists());
        assert!(dir.path().join("milestones.json").exists());
    }

    #[tokio::test]
    async fn repairs_logic_bug_at_point_d_and_passes() {
        let dir = tempdir().unwrap();
        let llm = MockLlm::new(vec![
            // Initial generation: wrong operator
            r#"{"files":[{"path":"src/math.py","content":"def add(a, b):\n    return a - b\n"}]}"#,
            // Point D surgical repair
            r#"{"patches":[{"path":"src/math.py","original_snippet":"def add(a, b):\n    return a - b","replacement_snippet":"def add(a, b):\n    return a + b"}]}"#,
        ]);

        let failing_test = vec![Diagnostic {
            file: "src/math.py".into(),
            line: 2,
            column: 5,
            code: Some("pytest_failure".into()),
            message: "assert -1 == 5".into(),
            related_spans: vec![],
            local_hint: None,
        }];
        // Gate 1 passes twice; Gate 2 fails once, then passes after patch
        let adapter = MockAdapter::new(vec![vec![], vec![]], vec![failing_test, vec![]]);
        let runner = SandboxRunner::unsandboxed(Duration::from_secs(5));
        let orch = Orchestrator::new(&llm, &adapter, &runner, OrchestratorConfig::default());

        let mut plan = sample_plan();
        let outcome = orch.run_project(dir.path(), &mut plan).await.unwrap();

        assert!(matches!(
            outcome,
            RunOutcome::CompletedAll { completed_count: 1 }
        ));
        assert_eq!(plan.milestones[0].attempts, 2);
        let fixed = fs::read_to_string(dir.path().join("src/math.py")).unwrap();
        assert!(fixed.contains("return a + b"));
    }

    #[tokio::test]
    async fn auto_installs_missing_dependency_without_burning_llm_turn() {
        let dir = tempdir().unwrap();
        let llm = MockLlm::new(vec![
            r#"{"files":[{"path":"src/client.py","content":"import httpx\n"}]}"#,
        ]);

        let missing_dep = vec![Diagnostic {
            file: "src/client.py".into(),
            line: 1,
            column: 8,
            code: Some("reportMissingImports".into()),
            message: r#"Import "httpx" could not be resolved"#.into(),
            related_spans: vec![],
            local_hint: None,
        }];
        // First static check reports missing httpx; second passes right after auto-install
        let adapter = MockAdapter::new(vec![missing_dep, vec![]], vec![vec![]]);
        let runner = SandboxRunner::unsandboxed(Duration::from_secs(5));
        let orch = Orchestrator::new(&llm, &adapter, &runner, OrchestratorConfig::default());

        let mut plan = sample_plan();
        let outcome = orch.run_project(dir.path(), &mut plan).await.unwrap();

        assert!(matches!(
            outcome,
            RunOutcome::CompletedAll { completed_count: 1 }
        ));
        assert_eq!(plan.milestones[0].attempts, 1); // Zero repair LLM turns spent
        assert_eq!(
            *adapter.installed.lock().unwrap(),
            vec![vec!["httpx".to_string()]]
        );
    }

    #[tokio::test]
    async fn stops_on_oscillation_builds_point_a_report_and_rolls_back() {
        let dir = tempdir().unwrap();
        let llm = MockLlm::new(vec![
            r#"{"files":[{"path":"src/app.py","content":"x: int = 'bad'\n"}]}"#,
            // Point D patch changes code but triggers the exact same diagnostic fingerprint
            r#"{"patches":[{"path":"src/app.py","original_snippet":"x: int = 'bad'","replacement_snippet":"x: int = 'still_bad'"}]}"#,
        ]);

        let persistent_err = vec![Diagnostic {
            file: "src/app.py".into(),
            line: 1,
            column: 10,
            code: Some("reportAssignmentType".into()),
            message: "Type str is not assignable to int".into(),
            related_spans: vec![],
            local_hint: None,
        }];
        let adapter = MockAdapter::new(vec![persistent_err.clone(), persistent_err], vec![]);
        let runner = SandboxRunner::unsandboxed(Duration::from_secs(5));
        let orch = Orchestrator::new(
            &llm,
            &adapter,
            &runner,
            OrchestratorConfig {
                auto_rollback_on_failure: true,
            },
        );

        let mut plan = sample_plan();
        let outcome = orch.run_project(dir.path(), &mut plan).await.unwrap();

        match outcome {
            RunOutcome::FailedAtMilestone(report) => {
                assert!(report.reason.contains("oscillation"));
                assert!(report.rolled_back);
                // Report captured the broken file BEFORE rollback deleted it
                assert!(report.copy_paste_prompt.contains("still_bad"));
                assert!(report.copy_paste_prompt.contains("reportAssignmentType"));
            }
            other => panic!("Expected FailedAtMilestone, got {other:?}"),
        }

        // Broken untracked file was wiped cleanly by rollback, while milestones.json survived
        assert!(!dir.path().join("src/app.py").exists());
        assert!(dir.path().join("milestones.json").exists());
        assert!(matches!(plan.milestones[0].status, TaskStatus::Failed(_)));
    }
}
