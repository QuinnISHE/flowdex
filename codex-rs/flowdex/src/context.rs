use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

use crate::workflow::{ContextFragmentSeed, ContextPackLifetime};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextPackDeclaration {
    pub agent: String,
    pub instructions: String,
    pub lifetime: ContextPackLifetime,
    pub fragments: Vec<ContextFragmentSeed>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextPublication {
    pub pack: String,
    pub key: String,
    pub path: PathBuf,
    pub line_start: u32,
    pub line_end: u32,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextPublisher {
    pub thread_id: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextPackStatus {
    Fresh,
    Missing,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextFragment {
    pub pack: String,
    pub key: String,
    pub version: i64,
    pub path: PathBuf,
    pub line_start: u32,
    pub line_end: u32,
    pub summary: Option<String>,
    pub content: String,
    pub content_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextStaleSource {
    pub key: String,
    pub path: PathBuf,
    pub line_start: u32,
    pub line_end: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedContextPack {
    pub pack: String,
    pub status: ContextPackStatus,
    pub fragments: Vec<ContextFragment>,
    pub stale_sources: Vec<ContextStaleSource>,
}

#[derive(Debug, Error)]
pub enum ContextError {
    #[error("context path must be repository-relative: {0}")]
    InvalidPath(PathBuf),
    #[error("context path escapes trusted root: {0}")]
    OutsideRoot(PathBuf),
    #[error("context source is a directory: {0}")]
    Directory(PathBuf),
    #[error("context source is a link or reparse point: {0}")]
    Reparse(PathBuf),
    #[error("context line range is invalid: {start}..={end}")]
    InvalidRange { start: u32, end: u32 },
    #[error("unable to read context source {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("context field is empty: {0}")]
    EmptyField(&'static str),
    #[error("invalid repository context pack `{0}`")]
    InvalidRepositoryPack(String),
    #[error("invalid repository context pack file {path}: {message}")]
    RepositoryFormat { path: PathBuf, message: String },
    #[error("unable to write repository context pack {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepositoryContextPackFile {
    version: u32,
    pack: String,
    fragments: Vec<RepositoryContextFragment>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepositoryContextFragment {
    key: String,
    version: i64,
    path: PathBuf,
    line_start: u32,
    line_end: u32,
    summary: Option<String>,
    content: String,
    content_hash: String,
}

pub(crate) fn validate_publication(publication: &ContextPublication) -> Result<(), ContextError> {
    if publication.pack.trim().is_empty() {
        return Err(ContextError::EmptyField("pack"));
    }
    if publication.key.trim().is_empty() {
        return Err(ContextError::EmptyField("key"));
    }
    validate_relative_path(&publication.path)?;
    if publication.line_start == 0 || publication.line_end < publication.line_start {
        return Err(ContextError::InvalidRange {
            start: publication.line_start,
            end: publication.line_end,
        });
    }
    Ok(())
}

pub(crate) fn validate_relative_path(path: &Path) -> Result<(), ContextError> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ContextError::InvalidPath(path.to_path_buf()));
    }
    Ok(())
}

pub(crate) fn read_repository_context_pack(
    root: &Path,
    pack: &str,
) -> Result<Option<Vec<ContextFragment>>, ContextError> {
    let path = repository_context_pack_path(root, pack)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ContextError::Read { path, source }),
    };
    if !metadata.is_file() || is_link_or_reparse(&metadata) {
        return Err(ContextError::Reparse(path));
    }
    let canonical_root = root.canonicalize().map_err(|source| ContextError::Read {
        path: root.to_path_buf(),
        source,
    })?;
    let canonical = path.canonicalize().map_err(|source| ContextError::Read {
        path: path.clone(),
        source,
    })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(ContextError::OutsideRoot(path));
    }
    let source = std::fs::read_to_string(&canonical).map_err(|source| ContextError::Read {
        path: canonical.clone(),
        source,
    })?;
    let stored: RepositoryContextPackFile =
        serde_json::from_str(&source).map_err(|error| ContextError::RepositoryFormat {
            path: canonical.clone(),
            message: error.to_string(),
        })?;
    if stored.version != 1 || stored.pack != pack {
        return Err(ContextError::RepositoryFormat {
            path: canonical,
            message: "unsupported version or mismatched pack name".into(),
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut fragments = Vec::with_capacity(stored.fragments.len());
    for fragment in stored.fragments {
        if fragment.key.trim().is_empty() || !seen.insert(fragment.key.clone()) {
            return Err(ContextError::RepositoryFormat {
                path: path.clone(),
                message: "fragment keys must be non-empty and unique".into(),
            });
        }
        validate_relative_path(&fragment.path)?;
        if fragment.line_start == 0 || fragment.line_end < fragment.line_start {
            return Err(ContextError::InvalidRange {
                start: fragment.line_start,
                end: fragment.line_end,
            });
        }
        fragments.push(ContextFragment {
            pack: pack.to_string(),
            key: fragment.key,
            version: fragment.version,
            path: fragment.path,
            line_start: fragment.line_start,
            line_end: fragment.line_end,
            summary: fragment.summary,
            content: fragment.content,
            content_hash: fragment.content_hash,
        });
    }
    Ok(Some(fragments))
}

pub(crate) fn write_repository_context_pack(
    root: &Path,
    pack: &str,
    fragments: &[ContextFragment],
) -> Result<(), ContextError> {
    let path = repository_context_pack_path(root, pack)?;
    let parent = path
        .parent()
        .ok_or_else(|| ContextError::InvalidRepositoryPack(pack.into()))?;
    std::fs::create_dir_all(parent).map_err(|source| ContextError::Write {
        path: parent.to_path_buf(),
        source,
    })?;
    let canonical_root = root.canonicalize().map_err(|source| ContextError::Read {
        path: root.to_path_buf(),
        source,
    })?;
    let canonical_parent = parent.canonicalize().map_err(|source| ContextError::Read {
        path: parent.to_path_buf(),
        source,
    })?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(ContextError::OutsideRoot(parent.to_path_buf()));
    }
    if let Ok(metadata) = std::fs::symlink_metadata(&path)
        && (!metadata.is_file() || is_link_or_reparse(&metadata))
    {
        return Err(ContextError::Reparse(path));
    }
    let stored = RepositoryContextPackFile {
        version: 1,
        pack: pack.to_string(),
        fragments: fragments
            .iter()
            .map(|fragment| RepositoryContextFragment {
                key: fragment.key.clone(),
                version: fragment.version,
                path: fragment.path.clone(),
                line_start: fragment.line_start,
                line_end: fragment.line_end,
                summary: fragment.summary.clone(),
                content: fragment.content.clone(),
                content_hash: fragment.content_hash.clone(),
            })
            .collect(),
    };
    let source =
        serde_json::to_string_pretty(&stored).map_err(|error| ContextError::RepositoryFormat {
            path: path.clone(),
            message: error.to_string(),
        })?;
    let temporary = parent.join(format!(".{}.{}.tmp", pack, uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        let mut file = options.open(&temporary)?;
        file.write_all(source.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        crate::atomic_replace(&temporary, &path)
    })();
    if let Err(source) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(ContextError::Write { path, source });
    }
    Ok(())
}

fn repository_context_pack_path(root: &Path, pack: &str) -> Result<PathBuf, ContextError> {
    if pack.is_empty()
        || matches!(pack, "." | "..")
        || !pack
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ContextError::InvalidRepositoryPack(pack.to_string()));
    }
    Ok(root
        .join(".flowdex")
        .join("context-packs")
        .join(format!("{pack}.json")))
}

fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        metadata.file_type().is_symlink()
    }
    #[cfg(windows)]
    {
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
}

pub(crate) fn read_source_range(
    root: &Path,
    relative_path: &Path,
    line_start: u32,
    line_end: u32,
) -> Result<String, ContextError> {
    validate_relative_path(relative_path)?;
    if line_start == 0 || line_end < line_start {
        return Err(ContextError::InvalidRange {
            start: line_start,
            end: line_end,
        });
    }
    let canonical_root = root.canonicalize().map_err(|source| ContextError::Read {
        path: root.to_path_buf(),
        source,
    })?;
    let candidate = root.join(relative_path);
    let canonical = candidate
        .canonicalize()
        .map_err(|source| ContextError::Read {
            path: candidate.clone(),
            source,
        })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(ContextError::OutsideRoot(relative_path.to_path_buf()));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    #[cfg(windows)]
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let mut file = options
        .open(&candidate)
        .map_err(|source| ContextError::Read {
            path: candidate.clone(),
            source,
        })?;
    let metadata = file.metadata().map_err(|source| ContextError::Read {
        path: candidate.clone(),
        source,
    })?;
    #[cfg(unix)]
    let is_reparse = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let is_reparse = metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    if metadata.is_dir() {
        return Err(ContextError::Directory(relative_path.to_path_buf()));
    }
    if is_reparse {
        return Err(ContextError::Reparse(relative_path.to_path_buf()));
    }
    let mut source = String::new();
    file.read_to_string(&mut source)
        .map_err(|source_error| ContextError::Read {
            path: candidate,
            source: source_error,
        })?;
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let line_start = line_start as usize;
    let line_end = line_end as usize;
    if line_end > lines.len() {
        return Err(ContextError::InvalidRange {
            start: line_start as u32,
            end: line_end as u32,
        });
    }
    Ok(lines[line_start - 1..line_end].concat())
}
