use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextPackDeclaration {
    pub agent: String,
    pub instructions: String,
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
    #[error("context line range is invalid: {start}..={end}")]
    InvalidRange { start: u32, end: u32 },
    #[error("unable to read context source {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("context field is empty: {0}")]
    EmptyField(&'static str),
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
    let metadata = canonical.metadata().map_err(|source| ContextError::Read {
        path: canonical.clone(),
        source,
    })?;
    if metadata.is_dir() {
        return Err(ContextError::Directory(relative_path.to_path_buf()));
    }
    let mut file = File::open(&canonical).map_err(|source| ContextError::Read {
        path: canonical,
        source,
    })?;
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
