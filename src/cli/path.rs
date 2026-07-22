use std::ffi::{OsStr, OsString};
use std::path::Path;

use super::completion::CompletionKind;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PathKind {
    File,
    Folder,
    Any,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PathAccess {
    Read,
    Write,
}

pub(super) fn matches(path: &Path, kind: PathKind, access: PathAccess) -> bool {
    match (std::fs::metadata(path), access) {
        (Ok(metadata), access) => {
            let kind_matches = match kind {
                PathKind::File => metadata.is_file(),
                PathKind::Folder => metadata.is_dir(),
                PathKind::Any => metadata.is_file() || metadata.is_dir(),
            };
            kind_matches && access_appears_usable(path, &metadata, access)
        }
        (Err(_), PathAccess::Read) => false,
        (Err(_), PathAccess::Write) => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            std::fs::metadata(parent)
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.permissions().readonly())
        }
    }
}

pub(super) fn expectation(kind: PathKind, access: PathAccess) -> &'static str {
    match (kind, access) {
        (PathKind::File, PathAccess::Read) => "readable file path",
        (PathKind::Folder, PathAccess::Read) => "readable folder path",
        (PathKind::Any, PathAccess::Read) => "readable path",
        (PathKind::File, PathAccess::Write) => "writable file path",
        (PathKind::Folder, PathAccess::Write) => "writable folder path",
        (PathKind::Any, PathAccess::Write) => "writable path",
    }
}

pub(super) fn completions(
    prefix: &OsStr,
    suffix: &OsStr,
    kind: PathKind,
    access: PathAccess,
) -> Vec<(OsString, CompletionKind, bool)> {
    let prefix_path = Path::new(prefix);
    let ends_with_separator = prefix
        .as_encoded_bytes()
        .ends_with(std::path::MAIN_SEPARATOR_STR.as_bytes());
    let (folder, name_prefix, explicit_parent) = if ends_with_separator {
        (prefix_path, OsStr::new(""), Some(prefix_path))
    } else {
        (
            prefix_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
            prefix_path.file_name().unwrap_or_default(),
            prefix_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty()),
        )
    };
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name
            .as_encoded_bytes()
            .starts_with(name_prefix.as_encoded_bytes())
        {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !access_appears_usable(&entry.path(), &metadata, access) {
            continue;
        }
        let is_folder = metadata.is_dir();
        if !is_folder && !matches!(kind, PathKind::File | PathKind::Any)
            || !is_folder && !metadata.is_file()
        {
            continue;
        }
        let mut replacement = explicit_parent
            .map(|parent| parent.join(&name).into_os_string())
            .unwrap_or_else(|| name.clone());
        if is_folder {
            replacement.push(std::path::MAIN_SEPARATOR_STR);
        }
        if !replacement
            .as_encoded_bytes()
            .ends_with(suffix.as_encoded_bytes())
        {
            continue;
        }
        let complete_reader = match kind {
            PathKind::File => !is_folder,
            PathKind::Folder => is_folder,
            PathKind::Any => true,
        };
        let candidate_kind = if is_folder {
            CompletionKind::Folder
        } else if kind == PathKind::Any {
            CompletionKind::Path
        } else {
            CompletionKind::File
        };
        candidates.push((replacement, candidate_kind, complete_reader));
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    candidates
}

fn access_appears_usable(path: &Path, metadata: &std::fs::Metadata, access: PathAccess) -> bool {
    match (metadata.is_file(), access) {
        (true, PathAccess::Read) => std::fs::File::open(path).is_ok(),
        (true, PathAccess::Write) => {
            !metadata.permissions().readonly()
                && std::fs::OpenOptions::new().write(true).open(path).is_ok()
        }
        (false, PathAccess::Read) => std::fs::read_dir(path).is_ok(),
        (false, PathAccess::Write) => !metadata.permissions().readonly(),
    }
}
