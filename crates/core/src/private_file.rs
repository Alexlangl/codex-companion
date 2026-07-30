use crate::{CompanionError, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::Builder;

/// Atomically replaces a credential file with owner-only permissions on Unix.
///
/// The complete payload is written and synced in the destination directory
/// before rename, so a failed write cannot truncate the previous credential.
pub fn atomic_write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&parent).map_err(|source| CompanionError::io(&parent, source))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("credential");
    let mut temporary = Builder::new()
        .prefix(&format!(".{file_name}."))
        .suffix(".tmp")
        .tempfile_in(&parent)
        .map_err(|source| CompanionError::io(&parent, source))?;

    set_private_permissions(temporary.as_file(), temporary.path())?;
    temporary
        .write_all(contents)
        .map_err(|source| CompanionError::io(temporary.path(), source))?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|source| CompanionError::io(temporary.path(), source))?;
    temporary
        .persist(path)
        .map_err(|error| CompanionError::io(path, error.error))?;
    sync_parent_dir(&parent)
}

#[cfg(unix)]
fn set_private_permissions(file: &File, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| CompanionError::io(path, source))
}

#[cfg(not(unix))]
fn set_private_permissions(_file: &File, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent_dir(parent: &Path) -> Result<()> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| CompanionError::io(parent, source))
}

#[cfg(not(unix))]
fn sync_parent_dir(_parent: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_existing_contents_without_leaving_a_partial_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("auth.json");
        fs::write(&path, b"old-secret").expect("seed auth");

        atomic_write_private_file(&path, b"new-secret").expect("replace auth");

        assert_eq!(fs::read(&path).expect("read auth"), b"new-secret");
        let leftovers = fs::read_dir(temp.path())
            .expect("read tempdir")
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[cfg(unix)]
    #[test]
    fn creates_and_replaces_files_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("auth.json");
        fs::write(&path, b"old-secret").expect("seed auth");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("loosen auth");

        atomic_write_private_file(&path, b"new-secret").expect("replace auth");

        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn replaces_a_symlink_without_overwriting_its_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("external-auth.json");
        let path = temp.path().join("auth.json");
        fs::write(&target, b"external-secret").expect("seed target");
        symlink(&target, &path).expect("create symlink");

        atomic_write_private_file(&path, b"companion-secret").expect("replace symlink");

        assert_eq!(fs::read(&target).expect("read target"), b"external-secret");
        assert_eq!(fs::read(&path).expect("read auth"), b"companion-secret");
        assert!(!fs::symlink_metadata(&path)
            .expect("symlink metadata")
            .file_type()
            .is_symlink());
    }
}
