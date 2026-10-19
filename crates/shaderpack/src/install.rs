//! Adds packs the player dropped onto the window to the shader-pack directory, as Iris's
//! shader pack screen does with dropped files.

use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    discover::{is_zip_name, pack_folder},
    source::zip_holds_pack,
};

/// Suffix of a copy still being written; [`crate::discover_packs`] skips such entries, so a
/// listing taken mid-copy never offers a partial pack.
pub(crate) const STAGING_SUFFIX: &str = ".importing";

/// What adding dropped files did. Like Iris, files that are not packs are skipped while any
/// dropped file is one, and the first pack that cannot be copied stops the rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropOutcome {
    /// These packs were copied in, in drop order.
    Added(Vec<String>),
    /// No dropped file is a pack; carries the file's name when exactly one was dropped.
    NoPacks(Option<String>),
    /// The directory already holds an entry with this pack's name; it was left untouched.
    AlreadyExists(String),
    /// Copying this pack failed with `error`; no partial copy remains.
    CopyFailed { name: String, error: String },
}

/// Whether `path` is a pack the shader-pack directory can list and open: a folder holding
/// `shaders/` directly or inside its single sub-folder, or a `.zip` whose root or single
/// top-level folder holds `shaders/`. Iris accepts a `shaders` folder at any depth; packs
/// nested deeper would be listed but fail to open here, so they are refused up front.
fn is_shader_pack(path: &Path) -> bool {
    if path.is_dir() {
        pack_folder(path.to_path_buf()).is_some()
    } else {
        is_zip_name(path) && zip_holds_pack(path)
    }
}

/// Copies each dropped pack in `paths` into `dir` under its own file or folder name,
/// creating `dir` when missing. Existing entries are never overwritten. Each copy is written
/// under a staging name and renamed into place once complete. Blocking file work: run it off
/// the frame.
pub fn add_dropped_packs(paths: &[PathBuf], dir: &Path) -> DropOutcome {
    let packs: Vec<_> = paths
        .iter()
        .filter_map(|path| Some((path, path.file_name()?.to_owned())))
        .filter(|(path, _)| is_shader_pack(path))
        .collect();
    if packs.is_empty() {
        let single = match paths {
            [path] => Some(display_name(path.file_name().unwrap_or(path.as_os_str()))),
            _ => None,
        };
        return DropOutcome::NoPacks(single);
    }
    let mut added = Vec::with_capacity(packs.len());
    for (path, file_name) in packs {
        let name = display_name(&file_name);
        let target = dir.join(&file_name);
        if fs::symlink_metadata(&target).is_ok() {
            return DropOutcome::AlreadyExists(name);
        }
        if let Err(error) = fs::create_dir_all(dir).and_then(|()| copy_pack(path, &target)) {
            return DropOutcome::CopyFailed {
                name,
                error: error.to_string(),
            };
        }
        added.push(name);
    }
    DropOutcome::Added(added)
}

fn display_name(name: &std::ffi::OsStr) -> String {
    name.to_string_lossy().into_owned()
}

/// Copies `source` (a zip or folder) to a staging sibling of `target`, then renames it into
/// place; a failed copy removes the staging entry.
fn copy_pack(source: &Path, target: &Path) -> io::Result<()> {
    let mut staging_name = target.file_name().map(OsString::from).unwrap_or_default();
    staging_name.push(STAGING_SUFFIX);
    let staging = target.with_file_name(staging_name);
    remove_entry(&staging)?;
    let copied = if source.is_dir() {
        copy_folder(source, &staging)
    } else {
        fs::copy(source, &staging).map(drop)
    }
    .and_then(|()| fs::rename(&staging, target));
    if copied.is_err() {
        let _ = remove_entry(&staging);
    }
    copied
}

/// Copies a folder tree; symlinks are skipped, as [`crate::PackSource::open`] skips them.
fn copy_folder(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let destination = to.join(entry.file_name());
        if kind.is_dir() {
            copy_folder(&entry.path(), &destination)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

/// Removes a file or folder tree at `path`; a missing entry is not an error.
fn remove_entry(path: &Path) -> io::Result<()> {
    let removed = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match removed {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests;
