use serde::{Deserialize, Serialize};
use sisu_core::{ModelMessage, ProjectPlan};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: u32 = 1;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Chat,
    Project,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Chat => "chat",
            Self::Project => "project",
        })
    }
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "chat" | "general" => Ok(Self::Chat),
            "project" => Ok(Self::Project),
            _ => Err("Mode must be chat or project.".into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub version: u32,
    pub id: String,
    pub workspace: PathBuf,
    pub mode: Mode,
    pub messages: Vec<ModelMessage>,
    pub plan: Option<ProjectPlan>,
    pub context_files: Vec<PathBuf>,
    pub created_at: u64,
    pub updated_at: u64,
    pub last_status: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub mode: Mode,
    pub updated_at: u64,
    pub message_count: usize,
    pub last_status: String,
}

pub struct SessionStore {
    workspace: PathBuf,
}

/// Holds an advisory operating-system lock for the entire open session.
/// Locks are released by the OS on process exit, including after a crash.
pub struct SessionHandle {
    pub session: Session,
    directory: PathBuf,
    _lock: File,
}

impl SessionStore {
    pub fn new(workspace: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
        }
    }

    fn root(&self, create: bool) -> Result<PathBuf, String> {
        let workspace = self
            .workspace
            .canonicalize()
            .map_err(|error| format!("Cannot resolve workspace: {error}"))?;
        let sisu = workspace.join(".sisu");
        let root = sisu.join("sessions");
        // Check existing ancestors before creating directories: a .sisu symlink
        // must not cause state writes outside the selected workspace.
        for ancestor in [&sisu, &root] {
            if ancestor.exists() {
                let canonical = ancestor
                    .canonicalize()
                    .map_err(|error| format!("Cannot resolve session storage: {error}"))?;
                if !canonical.starts_with(&workspace) {
                    return Err("Session storage must remain inside the selected workspace.".into());
                }
            }
        }
        if create {
            fs::create_dir_all(&root).map_err(|error| {
                format!("Cannot create session storage {}: {error}", root.display())
            })?;
        }
        Ok(root)
    }

    pub fn create(&self, mode: Mode) -> Result<SessionHandle, String> {
        let root = self.root(true)?;
        let directory = loop {
            let id = format!(
                "{:x}-{:x}-{:x}",
                now(),
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            );
            let directory = root.join(id);
            match fs::create_dir(&directory) {
                Ok(()) => break directory,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("Cannot create session: {error}")),
            }
        };
        let id = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let lock = lock_session(&directory)?;
        let timestamp = now();
        let mut handle = SessionHandle {
            session: Session {
                version: VERSION,
                id,
                workspace: self
                    .workspace
                    .canonicalize()
                    .map_err(|error| format!("Cannot resolve workspace: {error}"))?,
                mode,
                messages: Vec::new(),
                plan: None,
                context_files: Vec::new(),
                created_at: timestamp,
                updated_at: timestamp,
                last_status: "idle".into(),
                last_error: None,
            },
            directory,
            _lock: lock,
        };
        handle.save()?;
        Ok(handle)
    }

    pub fn load(&self, id: &str) -> Result<SessionHandle, String> {
        validate_id(id)?;
        let directory = self.root(false)?.join(id);
        let metadata = fs::symlink_metadata(&directory)
            .map_err(|error| format!("Cannot open session {id}: {error}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(
                "Session directory must be a real directory inside session storage.".into(),
            );
        }
        let lock = lock_session(&directory)?;
        let session = self.read(&directory, id)?;
        Ok(SessionHandle {
            session,
            directory,
            _lock: lock,
        })
    }

    pub fn list(&self) -> Result<Vec<SessionSummary>, String> {
        let root = self.root(false)?;
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("Cannot list sessions: {error}")),
        };
        let mut summaries = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| format!("Cannot read session entry: {error}"))?;
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            validate_id(&id)?;
            // A concurrent creator may hold the lock before its first atomic save.
            match fs::symlink_metadata(entry.path().join("session.json")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("Cannot inspect session {id}: {error}")),
                Ok(_) => {}
            }
            let session = self.read(&entry.path(), &id)?;
            summaries.push(SessionSummary {
                id: session.id,
                mode: session.mode,
                updated_at: session.updated_at,
                message_count: session.messages.len(),
                last_status: session.last_status,
            });
        }
        summaries.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then(left.id.cmp(&right.id))
        });
        Ok(summaries)
    }

    fn read(&self, directory: &Path, expected_id: &str) -> Result<Session, String> {
        let path = directory.join("session.json");
        check_regular_file(&path, false)?;
        let bytes = fs::read(&path)
            .map_err(|error| format!("Cannot read session {expected_id}: {error}"))?;
        let session: Session = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid session {expected_id}: {error}"))?;
        if session.version != VERSION {
            return Err(format!(
                "Session {expected_id} uses unsupported schema version {}; expected {VERSION}.",
                session.version
            ));
        }
        if session.id != expected_id {
            return Err(format!(
                "Session ID does not match its directory: {expected_id}."
            ));
        }
        let workspace = self
            .workspace
            .canonicalize()
            .map_err(|error| format!("Cannot resolve workspace: {error}"))?;
        if session.workspace != workspace {
            return Err(format!(
                "Session {expected_id} belongs to a different workspace."
            ));
        }
        if let Some(plan) = &session.plan {
            plan.validate()
                .map_err(|error| format!("Invalid saved plan in session {expected_id}: {error}"))?;
        }
        Ok(session)
    }
}

impl SessionHandle {
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn save(&mut self) -> Result<(), String> {
        self.session.updated_at = now();
        let bytes = serde_json::to_vec_pretty(&self.session)
            .map_err(|error| format!("Cannot encode session: {error}"))?;
        atomic_write(&self.directory.join("session.json"), &bytes)
    }
}

fn lock_session(directory: &Path) -> Result<File, String> {
    let path = directory.join("session.lock");
    check_regular_file(&path, true)?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|error| format!("Cannot open session lock: {error}"))?;
    lock.try_lock().map_err(|error| format!("Session is already open or cannot be locked: {error}. Close its other Sisu process before resuming."))?;
    Ok(lock)
}

fn check_regular_file(path: &Path, allow_missing: bool) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(format!(
            "Session state {} must be a regular file, not a symlink or special file.",
            path.display()
        )),
        Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Cannot inspect session state {}: {error}",
            path.display()
        )),
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("Invalid session ID; use an ID printed by `sisu sessions list`.".into());
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("State file must have a parent directory.")?;
    let temporary = parent.join(format!(
        ".session-{}-{}.tmp",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("Cannot save session {}: {error}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sisu-session-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn roundtrip_and_lock_release() {
        let workspace = fixture();
        let store = SessionStore::new(&workspace);
        let mut handle = store.create(Mode::Project).unwrap();
        let id = handle.session.id.clone();
        handle.session.last_status = "cancelled".into();
        handle
            .session
            .context_files
            .push(PathBuf::from("src/main.rs"));
        handle.save().unwrap();
        assert!(store.load(&id).is_err());
        assert_eq!(store.list().unwrap()[0].last_status, "cancelled");
        drop(handle);
        let restored = store.load(&id).unwrap();
        assert_eq!(
            restored.session.context_files,
            vec![PathBuf::from("src/main.rs")]
        );
        assert_eq!(restored.session.mode, Mode::Project);
        drop(restored);
        fs::remove_dir_all(workspace).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_outside_storage_before_creating_directories() {
        let workspace = fixture();
        let outside = fixture();
        std::os::unix::fs::symlink(&outside, workspace.join(".sisu")).unwrap();
        let store = SessionStore::new(&workspace);
        assert!(
            store
                .create(Mode::Chat)
                .err()
                .unwrap()
                .contains("inside the selected workspace")
        );
        assert!(!outside.join("sessions").exists());
        fs::remove_dir_all(workspace).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_state_and_lock_leaf_symlinks_including_dangling_links() {
        let workspace = fixture();
        let store = SessionStore::new(&workspace);
        let handle = store.create(Mode::Chat).unwrap();
        let id = handle.session.id.clone();
        let directory = handle.directory().to_path_buf();
        drop(handle);
        for leaf in ["session.json", "session.lock"] {
            let path = directory.join(leaf);
            let original = fs::read(&path).unwrap();
            fs::remove_file(&path).unwrap();
            for target in [directory.join("missing"), directory.join("target")] {
                if target.ends_with("target") {
                    fs::write(&target, &original).unwrap();
                }
                std::os::unix::fs::symlink(&target, &path).unwrap();
                assert!(store.load(&id).err().unwrap().contains("regular file"));
                fs::remove_file(&path).unwrap();
            }
            fs::write(&path, original).unwrap();
        }
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn validates_saved_plan_before_resume() {
        let workspace = fixture();
        let store = SessionStore::new(&workspace);
        let handle = store.create(Mode::Chat).unwrap();
        let id = handle.session.id.clone();
        let path = handle.directory().join("session.json");
        let mut invalid = serde_json::to_value(&handle.session).unwrap();
        drop(handle);
        invalid["plan"] = serde_json::json!({
            "goal": "invalid plan", "language": "rust", "check_command": "cargo check",
            "milestones": [{"id":"a", "title":"A", "description":"x", "depends_on":["missing"], "done_when":["done"]}]
        });
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            store
                .load(&id)
                .err()
                .unwrap()
                .contains("Invalid saved plan")
        );
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn rejects_paths_corrupt_state_and_wrong_workspace() {
        let workspace = fixture();
        let store = SessionStore::new(&workspace);
        assert!(store.load("../escape").is_err());
        let handle = store.create(Mode::Chat).unwrap();
        let id = handle.session.id.clone();
        let path = handle.directory().join("session.json");
        let mut invalid = serde_json::to_value(&handle.session).unwrap();
        drop(handle);
        invalid["version"] = serde_json::json!(999);
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            store
                .load(&id)
                .err()
                .unwrap()
                .contains("unsupported schema")
        );
        invalid["version"] = serde_json::json!(1);
        invalid["workspace"] = serde_json::json!("/different/workspace");
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            store
                .load(&id)
                .err()
                .unwrap()
                .contains("different workspace")
        );
        fs::write(&path, "{broken").unwrap();
        assert!(store.load(&id).err().unwrap().contains("Invalid session"));
        fs::remove_dir_all(workspace).unwrap();
    }
}
