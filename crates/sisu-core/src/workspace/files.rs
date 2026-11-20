use crate::models::{CodePatch, FileWrite};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Resolves a relative LLM path safely inside `project_dir`.
/// Blocks absolute paths and `..` directory escapes.
pub fn safe_resolve_path(project_dir: &Path, rel_path: &str) -> Result<PathBuf, String> {
    let rel = Path::new(rel_path);

    if rel.as_os_str().is_empty() {
        return Err("Empty file path".into());
    }

    for comp in rel.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("Unsafe path rejected by sandbox jail: {rel_path}"));
            }
        }
    }

    let canonical_root = project_dir
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize project_dir: {e}"))?;

    let target = canonical_root.join(rel);

    // Ensure parent directories exist, then verify canonical parent stays inside root
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create dirs for {rel_path}: {e}"))?;
        let canonical_parent = parent
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize parent for {rel_path}: {e}"))?;

        if !canonical_parent.starts_with(&canonical_root) {
            return Err(format!("Symlink/path escape blocked: {rel_path}"));
        }
    }

    Ok(target)
}

/// Writes generated files to disk and returns the list of written relative paths.
pub fn write_generated_files(
    project_dir: &Path,
    files: &[FileWrite],
) -> Result<Vec<String>, String> {
    let mut written = Vec::with_capacity(files.len());

    for file in files {
        let target = safe_resolve_path(project_dir, &file.path)?;
        fs::write(&target, &file.content)
            .map_err(|e| format!("Failed to write {}: {e}", file.path))?;
        written.push(file.path.clone());
    }

    Ok(written)
}

/// Applies surgical search-and-replace patches while blocking edits to locked test files.
pub fn apply_patches(
    project_dir: &Path,
    patches: &[CodePatch],
    locked_files: &[String],
) -> Result<(), String> {
    for patch in patches {
        if locked_files.iter().any(|f| f == &patch.path) {
            return Err(format!(
                "Repair blocked: '{}' is a locked test file",
                patch.path
            ));
        }

        let target = safe_resolve_path(project_dir, &patch.path)?;
        let content = fs::read_to_string(&target)
            .map_err(|e| format!("Failed to read {} for patching: {e}", patch.path))?;

        let match_count = content.matches(&patch.original_snippet).count();
        if match_count == 0 {
            return Err(format!(
                "Patch failed: original_snippet not found in {}",
                patch.path
            ));
        }
        if match_count > 1 {
            return Err(format!(
                "Patch ambiguous: original_snippet matched {match_count} times in {}",
                patch.path
            ));
        }

        let updated = content.replacen(&patch.original_snippet, &patch.replacement_snippet, 1);
        fs::write(&target, updated)
            .map_err(|e| format!("Failed to write patched {}: {e}", patch.path))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn blocks_parent_dir_traversal() {
        let dir = tempdir().unwrap();
        let res = safe_resolve_path(dir.path(), "../escape.txt");
        assert!(res.is_err());
    }

    #[test]
    fn blocks_absolute_path() {
        let dir = tempdir().unwrap();
        let res = safe_resolve_path(dir.path(), "/tmp/escape.txt");
        assert!(res.is_err());
    }

    #[test]
    fn writes_and_patches_valid_file() {
        let dir = tempdir().unwrap();
        let files = vec![FileWrite {
            path: "src/math.py".into(),
            content: "def add(a, b):\n    return a - b\n".into(),
        }];

        let written = write_generated_files(dir.path(), &files).unwrap();
        assert_eq!(written, vec!["src/math.py"]);

        let patches = vec![CodePatch {
            path: "src/math.py".into(),
            original_snippet: "def add(a, b):\n    return a - b".into(),
            replacement_snippet: "def add(a, b):\n    return a + b".into(),
        }];

        apply_patches(dir.path(), &patches, &[]).unwrap();

        let content = fs::read_to_string(dir.path().join("src/math.py")).unwrap();
        assert!(content.contains("return a + b"));
    }

    #[test]
    fn blocks_editing_locked_test_file() {
        let dir = tempdir().unwrap();
        let files = vec![FileWrite {
            path: "tests/test_math.py".into(),
            content: "assert add(2, 3) == 5\n".into(),
        }];
        write_generated_files(dir.path(), &files).unwrap();

        let patches = vec![CodePatch {
            path: "tests/test_math.py".into(),
            original_snippet: "assert add(2, 3) == 5".into(),
            replacement_snippet: "assert add(2, 3) == -1".into(),
        }];

        let locked = vec!["tests/test_math.py".into()];
        let res = apply_patches(dir.path(), &patches, &locked);
        assert!(res.is_err());

        // Verify the test file stayed untouched
        let content = fs::read_to_string(dir.path().join("tests/test_math.py")).unwrap();
        assert!(content.contains("== 5"));
    }

    #[test]
    fn rejects_ambiguous_snippet_match() {
        let dir = tempdir().unwrap();
        let files = vec![FileWrite {
            path: "src/dup.py".into(),
            content: "def f():\n    return 0\n\ndef g():\n    return 0\n".into(),
        }];
        write_generated_files(dir.path(), &files).unwrap();

        let patches = vec![CodePatch {
            path: "src/dup.py".into(),
            original_snippet: "return 0".into(),
            replacement_snippet: "return 1".into(),
        }];

        let res = apply_patches(dir.path(), &patches, &[]);
        assert!(res.is_err());
    }
}
