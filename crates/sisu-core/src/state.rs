use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::models::ProjectPlan;

#[derive(Debug)]
pub enum PlanError {
    Json(serde_json::Error),
    Invalid(String),
    Io(std::io::Error),
}
impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "Plan JSON is invalid: {error}"),
            Self::Invalid(message) => write!(f, "Plan is invalid: {message}"),
            Self::Io(error) => write!(f, "Could not read or save plan: {error}"),
        }
    }
}
impl Error for PlanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::Invalid(_) => None,
        }
    }
}
impl From<std::io::Error> for PlanError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for PlanError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub fn parse_project_plan(raw_output: &str) -> Result<ProjectPlan, PlanError> {
    let trimmed = raw_output.trim();
    let without_prefix = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let cleaned = without_prefix
        .strip_suffix("```")
        .unwrap_or(without_prefix)
        .trim();
    let plan = serde_json::from_str::<ProjectPlan>(cleaned)?;
    validate_project_plan(&plan)?;
    Ok(plan)
}

/// Validate the whole dependency graph independently of milestone ordering.
/// Kahn's algorithm avoids recursive traversal of model-generated input.
pub fn validate_project_plan(plan: &ProjectPlan) -> Result<(), PlanError> {
    for (name, value) in [
        ("goal", &plan.goal),
        ("language", &plan.language),
        ("check_command", &plan.check_command),
    ] {
        if value.trim().is_empty() {
            return Err(PlanError::Invalid(format!("{name} cannot be empty")));
        }
    }
    if plan.milestones.is_empty() || plan.milestones.len() > 4096 {
        return Err(PlanError::Invalid(
            "a plan must contain between 1 and 4096 milestones".into(),
        ));
    }
    let mut indices = HashMap::new();
    for (index, milestone) in plan.milestones.iter().enumerate() {
        if milestone.id.trim().is_empty()
            || milestone.id.trim() != milestone.id
            || milestone.id.len() > 128
        {
            return Err(PlanError::Invalid("milestone IDs must be nonempty, at most 128 bytes, and have no surrounding whitespace".into()));
        }
        if indices.insert(milestone.id.as_str(), index).is_some() {
            return Err(PlanError::Invalid(format!(
                "duplicate milestone ID {}",
                milestone.id
            )));
        }
        if milestone.title.trim().is_empty() || milestone.description.trim().is_empty() {
            return Err(PlanError::Invalid(format!(
                "milestone {} requires a title and description",
                milestone.id
            )));
        }
        if milestone.done_when.is_empty()
            || milestone
                .done_when
                .iter()
                .any(|value| value.trim().is_empty())
        {
            return Err(PlanError::Invalid(format!(
                "milestone {} requires nonempty completion criteria",
                milestone.id
            )));
        }
    }
    let mut remaining = vec![0usize; plan.milestones.len()];
    let mut dependents = vec![Vec::new(); plan.milestones.len()];
    for (index, milestone) in plan.milestones.iter().enumerate() {
        let mut seen = HashSet::new();
        for dependency in &milestone.depends_on {
            let dependency_index = *indices.get(dependency.as_str()).ok_or_else(|| {
                PlanError::Invalid(format!(
                    "milestone {} references missing dependency {dependency}",
                    milestone.id
                ))
            })?;
            if dependency_index == index {
                return Err(PlanError::Invalid(format!(
                    "milestone {} depends on itself",
                    milestone.id
                )));
            }
            if !seen.insert(dependency) {
                return Err(PlanError::Invalid(format!(
                    "milestone {} repeats dependency {dependency}",
                    milestone.id
                )));
            }
            remaining[index] += 1;
            dependents[dependency_index].push(index);
        }
    }
    let mut ready: VecDeque<usize> = remaining
        .iter()
        .enumerate()
        .filter_map(|(i, degree)| (*degree == 0).then_some(i))
        .collect();
    let mut visited = 0usize;
    while let Some(index) = ready.pop_front() {
        visited += 1;
        for dependent in &dependents[index] {
            remaining[*dependent] -= 1;
            if remaining[*dependent] == 0 {
                ready.push_back(*dependent);
            }
        }
    }
    if visited != plan.milestones.len() {
        return Err(PlanError::Invalid(
            "milestone dependency graph contains a cycle".into(),
        ));
    }
    Ok(())
}

impl ProjectPlan {
    pub fn validate(&self) -> Result<(), PlanError> {
        validate_project_plan(self)
    }
}

pub fn load_plan(project_dir: impl AsRef<Path>) -> Result<ProjectPlan, PlanError> {
    parse_project_plan(&fs::read_to_string(
        project_dir.as_ref().join("milestones.json"),
    )?)
}

pub fn persist_plan(
    project_dir: impl AsRef<Path>,
    plan: &ProjectPlan,
) -> Result<PathBuf, PlanError> {
    validate_project_plan(plan)?;
    let dir = project_dir.as_ref();
    fs::create_dir_all(dir)?;
    let destination = dir.join("milestones.json");
    // Keep the same lock inode across saves: unlinking it could allow two
    // independent writers to lock different files. The OS releases the lock
    // when this handle closes, including after a process crash.
    let lock_path = dir.join("milestones.json.lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    lock_file.try_lock().map_err(|error| {
        PlanError::Io(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "{} is held by another writer or cannot be locked: {error}",
                lock_path.display()
            ),
        ))
    })?;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temp = dir.join(format!(
        ".milestones-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let _temp = Cleanup(temp.clone());
    let bytes = serde_json::to_vec_pretty(plan)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, &destination)?;
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    Ok(destination)
}
