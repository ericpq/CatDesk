//! Pre-image snapshots for the workspace-editing tools.
//!
//! `write`, `edit`, `delete` and `apply_patch` record the files they are about
//! to touch before touching them, so a wrong edit can be undone. Instructing a
//! model not to lose work is not the same as being able to get it back, and the
//! workspace is often not a clean Git tree — or not a repository at all.
//!
//! Only the paths a tool actually names are captured. `run_command` is not
//! covered: what a shell command will touch is not knowable in advance, and
//! snapshotting the whole workspace on every call would cost more than it saves.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::macros::format_description;

const CHECKPOINT_DIR: &str = ".catdesk/checkpoints";
/// A single file larger than this is recorded as skipped rather than copied,
/// so one large asset cannot fill the disk with undo history.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CHECKPOINT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 500;
const MAX_CHECKPOINTS: usize = 12;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// The path held a file whose contents were copied into the checkpoint.
    File,
    /// The path did not exist, so undoing means removing whatever is there now.
    Absent,
    /// The path existed but was not captured; restoring must not touch it.
    Skipped,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointEntry {
    pub path: String,
    pub kind: EntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub tool: String,
    pub created_at: String,
    pub created_at_ms: u64,
    pub entries: Vec<CheckpointEntry>,
}

impl Checkpoint {
    pub fn captured_paths(&self) -> Vec<&str> {
        self.entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    pub removed: Vec<String>,
    pub skipped: Vec<String>,
}

fn checkpoints_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join(CHECKPOINT_DIR)
}

fn now() -> (String, u64) {
    let now = OffsetDateTime::now_utc();
    let text = now
        .format(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
        ))
        .unwrap_or_else(|_| String::from("unknown"));
    let millis = (now.unix_timestamp_nanos() / 1_000_000).max(0) as u64;
    (text, millis)
}

/// Reject a stored path that could write outside the workspace. Manifests live
/// in the workspace and are therefore reachable by the very tools this module
/// protects against, so their contents are treated as untrusted on the way out.
fn safe_relative(path: &str) -> Option<PathBuf> {
    let candidate = Path::new(path);
    // `root.join("")` is the root, so an empty path would ask an `Absent` entry
    // to delete the whole workspace.
    if path.is_empty() || candidate.is_absolute() {
        return None;
    }
    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return None;
    }
    Some(candidate.to_path_buf())
}

fn relative_to_root(workspace_root: &Path, path: &Path) -> Option<String> {
    let root = workspace_root.canonicalize().ok()?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    // The root has no name to append to a canonical parent, and its parent sits
    // outside the workspace. It is still a legitimate capture target: `delete .`
    // and a whole-workspace scope both resolve to it.
    if absolute.canonicalize().ok().as_deref() == Some(root.as_path()) {
        return Some(String::new());
    }
    // Canonicalize the parent rather than the path: the path may not exist yet,
    // and a symlink belongs at its own location, not at its target's.
    let parent = absolute.parent()?.canonicalize().ok()?;
    let name = absolute.file_name()?;
    parent
        .join(name)
        .strip_prefix(&root)
        .ok()
        .map(|value| value.to_string_lossy().replace('\\', "/"))
}

/// Collect the files under `path`, or the file itself, into `entries`.
fn collect(
    workspace_root: &Path,
    path: &Path,
    entries: &mut Vec<(String, EntryKind, Option<PathBuf>, u64, Option<String>)>,
    total_bytes: &mut u64,
) {
    if entries.len() >= MAX_ENTRIES {
        return;
    }
    let Some(relative) = relative_to_root(workspace_root, path) else {
        return;
    };
    // CatDesk's own storage is not user work, and capturing it would make every
    // checkpoint contain the previous one.
    if relative == ".catdesk" || relative.starts_with(".catdesk/") {
        return;
    }

    let Ok(metadata) = fs::symlink_metadata(path) else {
        entries.push((relative, EntryKind::Absent, None, 0, None));
        return;
    };

    if metadata.is_symlink() {
        entries.push((
            relative,
            EntryKind::Skipped,
            None,
            0,
            Some("symlink".to_string()),
        ));
        return;
    }

    if metadata.is_dir() {
        let Ok(children) = fs::read_dir(path) else {
            return;
        };
        // A directory is only ever recursed into, never recorded itself, so the
        // empty relative path of the workspace root stops here.
        // Sorted so a checkpoint of the same directory is reproducible.
        let mut child_paths: Vec<PathBuf> = children
            .filter_map(|entry| entry.ok())
            .map(|e| e.path())
            .collect();
        child_paths.sort();
        for child in child_paths {
            collect(workspace_root, &child, entries, total_bytes);
        }
        return;
    }

    if relative.is_empty() {
        return;
    }

    let size = metadata.len();
    if size > MAX_FILE_BYTES || *total_bytes + size > MAX_CHECKPOINT_BYTES {
        entries.push((
            relative,
            EntryKind::Skipped,
            None,
            size,
            Some("too large to capture".to_string()),
        ));
        return;
    }
    *total_bytes += size;
    entries.push((
        relative,
        EntryKind::File,
        Some(path.to_path_buf()),
        size,
        None,
    ));
}

/// Record the current state of `paths` and return the stored checkpoint.
///
/// Returns `None` when there is nothing worth recording, so a caller can treat
/// a missing checkpoint as "no undo available" rather than as an error.
pub fn capture(workspace_root: &Path, tool: &str, paths: &[PathBuf]) -> Option<Checkpoint> {
    let mut seen = BTreeSet::new();
    let mut collected = Vec::new();
    let mut total_bytes = 0u64;
    for path in paths {
        if !seen.insert(path.clone()) {
            continue;
        }
        collect(workspace_root, path, &mut collected, &mut total_bytes);
    }
    if collected.is_empty() {
        return None;
    }

    let (created_at, created_at_ms) = now();
    let id = uuid::Uuid::new_v4().to_string();
    let directory = checkpoints_root(workspace_root).join(&id);
    let blobs = directory.join("blobs");
    if fs::create_dir_all(&blobs).is_err() {
        return None;
    }

    let mut entries = Vec::with_capacity(collected.len());
    for (index, (path, kind, source, bytes, reason)) in collected.into_iter().enumerate() {
        let mut entry = CheckpointEntry {
            path,
            kind,
            blob: None,
            bytes,
            reason,
        };
        if let Some(source) = source {
            let blob = index.to_string();
            if fs::copy(&source, blobs.join(&blob)).is_ok() {
                entry.blob = Some(blob);
            } else {
                // The file exists but could not be read; saying so is better
                // than a checkpoint that silently cannot restore it.
                entry.kind = EntryKind::Skipped;
                entry.reason = Some("unreadable".to_string());
            }
        }
        entries.push(entry);
    }

    let checkpoint = Checkpoint {
        id,
        tool: tool.to_string(),
        created_at,
        created_at_ms,
        entries,
    };
    let manifest = match serde_json::to_string_pretty(&checkpoint) {
        Ok(manifest) => manifest,
        Err(_) => return None,
    };
    if fs::write(directory.join("manifest.json"), manifest).is_err() {
        let _ = fs::remove_dir_all(&directory);
        return None;
    }
    prune(workspace_root);
    Some(checkpoint)
}

/// Newest first.
pub fn list(workspace_root: &Path) -> Vec<Checkpoint> {
    let Ok(entries) = fs::read_dir(checkpoints_root(workspace_root)) else {
        return Vec::new();
    };
    let mut checkpoints: Vec<Checkpoint> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let manifest = fs::read_to_string(entry.path().join("manifest.json")).ok()?;
            serde_json::from_str::<Checkpoint>(&manifest).ok()
        })
        .collect();
    checkpoints.sort_by(|left, right| right.created_at_ms.cmp(&left.created_at_ms));
    checkpoints
}

fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| match entry.metadata() {
            Ok(metadata) if metadata.is_dir() => directory_bytes(&entry.path()),
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Keep the newest checkpoints within both a count and a size budget, so undo
/// history cannot grow without bound in a long-running workspace.
fn prune(workspace_root: &Path) {
    let root = checkpoints_root(workspace_root);
    let mut kept_bytes = 0u64;
    for (index, checkpoint) in list(workspace_root).iter().enumerate() {
        let directory = root.join(&checkpoint.id);
        let bytes = directory_bytes(&directory);
        if index >= MAX_CHECKPOINTS || kept_bytes + bytes > MAX_TOTAL_BYTES {
            let _ = fs::remove_dir_all(&directory);
            continue;
        }
        kept_bytes += bytes;
    }
}

/// Put the workspace back the way `id` (or the newest checkpoint) found it.
pub fn restore(
    workspace_root: &Path,
    id: Option<&str>,
) -> Result<(Checkpoint, RestoreReport), String> {
    let checkpoints = list(workspace_root);
    let checkpoint = match id {
        Some(id) => checkpoints
            .into_iter()
            .find(|checkpoint| checkpoint.id == id)
            .ok_or_else(|| format!("No checkpoint with id {id}"))?,
        None => checkpoints
            .into_iter()
            .next()
            .ok_or_else(|| "No checkpoint has been recorded yet".to_string())?,
    };

    let blobs = checkpoints_root(workspace_root)
        .join(&checkpoint.id)
        .join("blobs");
    let mut report = RestoreReport::default();
    for entry in &checkpoint.entries {
        let Some(relative) = safe_relative(&entry.path) else {
            report.skipped.push(entry.path.clone());
            continue;
        };
        let target = workspace_root.join(&relative);
        match entry.kind {
            EntryKind::File => {
                let Some(blob) = entry.blob.as_ref() else {
                    report.skipped.push(entry.path.clone());
                    continue;
                };
                let restored = target
                    .parent()
                    .map(|parent| fs::create_dir_all(parent))
                    .unwrap_or(Ok(()))
                    .and_then(|_| fs::copy(blobs.join(blob), &target).map(|_| ()));
                match restored {
                    Ok(()) => report.restored.push(entry.path.clone()),
                    Err(_) => report.skipped.push(entry.path.clone()),
                }
            }
            EntryKind::Absent => match remove_path(&target) {
                Ok(true) => report.removed.push(entry.path.clone()),
                Ok(false) => {}
                Err(_) => report.skipped.push(entry.path.clone()),
            },
            EntryKind::Skipped => report.skipped.push(entry.path.clone()),
        }
    }

    Ok((checkpoint, report))
}

fn remove_path(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Err(_) => Ok(false),
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path).map(|_| true),
        Ok(_) => fs::remove_file(path).map(|_| true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "catdesk-checkpoint-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("create workspace");
        root.canonicalize().expect("canonical workspace")
    }

    #[test]
    fn restoring_puts_back_an_overwritten_file() {
        let root = workspace("overwrite");
        let file = root.join("notes.txt");
        std::fs::write(&file, "before\n").expect("seed file");

        let checkpoint =
            capture(&root, "write", &[file.clone()]).expect("checkpoint was not recorded");
        assert_eq!(checkpoint.captured_paths(), vec!["notes.txt"]);
        std::fs::write(&file, "after\n").expect("overwrite");

        let (_, report) = restore(&root, None).expect("restore");
        assert_eq!(report.restored, vec!["notes.txt".to_string()]);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "before\n");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restoring_removes_a_file_that_did_not_exist_before() {
        let root = workspace("created");
        let file = root.join("new.txt");

        capture(&root, "write", &[file.clone()]).expect("checkpoint was not recorded");
        std::fs::write(&file, "created\n").expect("create");

        let (_, report) = restore(&root, None).expect("restore");
        assert_eq!(report.removed, vec!["new.txt".to_string()]);
        assert!(!file.exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_deleted_directory_comes_back_with_every_file_in_it() {
        let root = workspace("tree");
        let dir = root.join("pkg");
        std::fs::create_dir_all(dir.join("inner")).expect("create tree");
        std::fs::write(dir.join("a.txt"), "a\n").expect("write a");
        std::fs::write(dir.join("inner/b.txt"), "b\n").expect("write b");

        capture(&root, "delete", &[dir.clone()]).expect("checkpoint was not recorded");
        std::fs::remove_dir_all(&dir).expect("delete tree");

        let (_, report) = restore(&root, None).expect("restore");
        assert_eq!(report.restored.len(), 2);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "a\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("inner/b.txt")).unwrap(),
            "b\n"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn checkpoints_are_listed_newest_first_and_can_be_restored_by_id() {
        let root = workspace("by-id");
        let file = root.join("notes.txt");
        std::fs::write(&file, "one\n").expect("seed");
        let first = capture(&root, "write", &[file.clone()]).expect("first checkpoint");
        std::fs::write(&file, "two\n").expect("second write");
        let second = capture(&root, "write", &[file.clone()]).expect("second checkpoint");
        std::fs::write(&file, "three\n").expect("third write");

        let listed = list(&root);
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, second.id, "newest checkpoint comes first");

        let (used, _) = restore(&root, Some(&first.id)).expect("restore by id");
        assert_eq!(used.id, first.id);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\n");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_path_outside_the_workspace_is_never_captured() {
        let root = workspace("escape");
        let outside = root.parent().expect("parent").join("outside.txt");
        std::fs::write(&outside, "outside\n").expect("seed outside");

        assert!(capture(&root, "write", &[outside.clone()]).is_none());

        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_manifest_path_that_escapes_the_workspace_is_refused_on_restore() {
        let root = workspace("tampered");
        let file = root.join("notes.txt");
        std::fs::write(&file, "before\n").expect("seed");
        let checkpoint = capture(&root, "write", &[file]).expect("checkpoint");

        // A manifest lives in the workspace, so an edit tool can reach it.
        let manifest = checkpoints_root(&root)
            .join(&checkpoint.id)
            .join("manifest.json");
        let text = std::fs::read_to_string(&manifest).expect("read manifest");
        std::fs::write(&manifest, text.replace("notes.txt", "../escaped.txt"))
            .expect("tamper with manifest");

        let (_, report) = restore(&root, None).expect("restore");
        assert_eq!(report.restored, Vec::<String>::new());
        assert_eq!(report.skipped, vec!["../escaped.txt".to_string()]);
        assert!(!root.parent().expect("parent").join("escaped.txt").exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn capturing_the_workspace_root_records_the_files_under_it() {
        let root = workspace("root-scope");
        std::fs::write(root.join("a.txt"), "a\n").expect("write a");
        std::fs::create_dir_all(root.join("pkg")).expect("create pkg");
        std::fs::write(root.join("pkg/b.txt"), "b\n").expect("write b");

        let checkpoint = capture(&root, "apply_patch", &[root.clone()]).expect("checkpoint");
        let mut paths = checkpoint.captured_paths();
        paths.sort();
        assert_eq!(paths, vec!["a.txt", "pkg/b.txt"]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn checkpoint_storage_does_not_capture_itself() {
        let root = workspace("self");
        std::fs::write(root.join("notes.txt"), "before\n").expect("seed");
        capture(&root, "write", &[root.join("notes.txt")]).expect("first checkpoint");

        // Capturing the whole workspace must not fold the previous checkpoint in.
        let second = capture(&root, "apply_patch", &[root.clone()]).expect("second checkpoint");
        assert!(
            second
                .entries
                .iter()
                .all(|entry| !entry.path.starts_with(".catdesk")),
            "checkpoint captured its own storage: {:?}",
            second.captured_paths()
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restoring_without_any_checkpoint_is_an_error_not_a_panic() {
        let root = workspace("empty");
        assert!(restore(&root, None).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
