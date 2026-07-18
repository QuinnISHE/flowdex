pub mod config;

pub use config::DEFAULT_COMPACTION_REMINDER_THRESHOLD_TOKENS;
pub use config::FlowdexConfig;
pub use config::FlowdexConfigError;
pub use config::load_config;

use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use thiserror::Error;

const WORKFLOW_DIRECTORY: [&str; 2] = [".flowdex", "workflows"];

/// Loads a repository workflow and prepares it for execution in code mode.
#[derive(Debug, Clone)]
pub struct WorkflowLoader {
    repository_root: AbsolutePathBuf,
}

impl WorkflowLoader {
    pub fn new(repository_root: AbsolutePathBuf) -> Self {
        Self { repository_root }
    }

    pub fn load(
        &self,
        workflow_path: &Path,
        input: Option<&Value>,
    ) -> Result<LoadedWorkflow, WorkflowLoadError> {
        let relative_path = normalize_workflow_path(workflow_path)?;
        let repository_root = self
            .repository_root
            .canonicalize()
            .map_err(WorkflowLoadError::repository_root)?;
        let workflow_root = repository_root
            .join(WORKFLOW_DIRECTORY[0])
            .join(WORKFLOW_DIRECTORY[1])
            .canonicalize()
            .map_err(WorkflowLoadError::workflow_root)?;
        if !workflow_root.starts_with(repository_root.as_path()) {
            return Err(WorkflowLoadError::OutsideWorkflowRoot);
        }

        let target = workflow_root.join(relative_path_tail(&relative_path));
        let canonical_target = target
            .canonicalize()
            .map_err(WorkflowLoadError::workflow_file)?;
        if !canonical_target.starts_with(workflow_root.as_path()) {
            return Err(WorkflowLoadError::OutsideWorkflowRoot);
        }

        let mut workflow_file =
            open_workflow_file(&canonical_target).map_err(WorkflowLoadError::workflow_file)?;
        if !workflow_file
            .metadata()
            .map_err(WorkflowLoadError::read_workflow)?
            .is_file()
        {
            return Err(WorkflowLoadError::NotRegularFile);
        }
        let mut source = String::new();
        workflow_file
            .read_to_string(&mut source)
            .map_err(WorkflowLoadError::read_workflow)?;
        let input = serde_json::to_string(input.unwrap_or(&Value::Null))
            .map_err(WorkflowLoadError::serialize_bootstrap)?;
        let workflow_path = workflow_display_path(&relative_path)?;
        let workflow_path = serde_json::to_string(&workflow_path)
            .map_err(WorkflowLoadError::serialize_bootstrap)?;

        Ok(LoadedWorkflow {
            source: format!(
                "const flowdex = Object.freeze({{\n  input: {input},\n  workflowPath: {workflow_path},\n  spawnAgent: async (spec) => tools.flowdex_spawn_agent({{\n    name: spec.name,\n    instructions: spec.instructions,\n    profile: spec.profile,\n    model: spec.model,\n    reasoning_effort: spec.reasoningEffort,\n  }}),\n  sendMessage: async (agentId, message, options = {{}}) => tools.flowdex_send_message({{\n    agent_id: agentId,\n    message,\n    delivery: options.delivery ?? \"queue\",\n  }}),\n  waitAgent: async (agentId) => tools.flowdex_wait_agent({{ agent_id: agentId }}),\n  progress: async (summary) => {{\n    await tools.flowdex_progress({{ summary }});\n  }},\n  verify: async (commands, options = {{}}) => tools.flowdex_verify({{\n    commands,\n    workdir: options.workdir,\n    timeout_ms: options.timeoutMs,\n  }}),\n}});\n\n{source}"
            ).replace(
                "  progress:",
                "  resumeAgent: async (agentId, instructions, options = {}) => tools.flowdex_resume_agent({ agent_id: agentId, instructions, options: { context_mode: options.contextMode } }),\n  progress:",
            ),
        })
    }
}

#[derive(Debug, Clone)]
pub struct LoadedWorkflow {
    pub source: String,
}

impl LoadedWorkflow {
    pub fn into_source(self) -> String {
        self.source
    }
}

#[derive(Debug, Error)]
pub enum WorkflowLoadError {
    #[error("invalid workflow path")]
    InvalidPath,
    #[error("workflow path is outside .flowdex/workflows")]
    OutsideWorkflowRoot,
    #[error("workflow must use the .js extension")]
    InvalidExtension,
    #[error("workflow root is unavailable")]
    WorkflowRootUnavailable,
    #[error("workflow file was not found")]
    WorkflowNotFound,
    #[error("workflow is not a regular file")]
    NotRegularFile,
    #[error("unable to read workflow")]
    ReadFailed,
    #[error("workflow is not valid UTF-8")]
    InvalidUtf8,
    #[error("unable to serialize workflow bootstrap")]
    BootstrapSerialization,
}

impl WorkflowLoadError {
    fn repository_root(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::NotFound {
            Self::WorkflowRootUnavailable
        } else {
            Self::ReadFailed
        }
    }

    fn workflow_root(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::NotFound {
            Self::WorkflowRootUnavailable
        } else {
            Self::ReadFailed
        }
    }

    fn workflow_file(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::NotFound {
            Self::WorkflowNotFound
        } else {
            Self::ReadFailed
        }
    }

    fn read_workflow(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::InvalidData {
            Self::InvalidUtf8
        } else {
            Self::ReadFailed
        }
    }

    fn serialize_bootstrap(_: serde_json::Error) -> Self {
        Self::BootstrapSerialization
    }
}

fn normalize_workflow_path(path: &Path) -> Result<PathBuf, WorkflowLoadError> {
    if path.is_absolute() {
        return Err(WorkflowLoadError::InvalidPath);
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WorkflowLoadError::InvalidPath);
            }
        }
    }

    let mut components = normalized.components();
    if !matches!(
        components.next(),
        Some(Component::Normal(component)) if component == WORKFLOW_DIRECTORY[0]
    ) || !matches!(
        components.next(),
        Some(Component::Normal(component)) if component == WORKFLOW_DIRECTORY[1]
    ) {
        return Err(WorkflowLoadError::OutsideWorkflowRoot);
    }
    if components.next().is_none() {
        return Err(WorkflowLoadError::InvalidPath);
    }
    if normalized.extension().and_then(|ext| ext.to_str()) != Some("js") {
        return Err(WorkflowLoadError::InvalidExtension);
    }
    if normalized.to_str().is_none() {
        return Err(WorkflowLoadError::InvalidPath);
    }
    Ok(normalized)
}

fn relative_path_tail(path: &Path) -> PathBuf {
    path.components().skip(WORKFLOW_DIRECTORY.len()).collect()
}

fn open_workflow_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn workflow_display_path(path: &Path) -> Result<String, WorkflowLoadError> {
    path.components()
        .map(|component| match component {
            Component::Normal(part) => part.to_str().ok_or(WorkflowLoadError::InvalidPath),
            _ => Err(WorkflowLoadError::InvalidPath),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|components| components.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    fn loader_with_workflow(source: &str) -> (tempfile::TempDir, WorkflowLoader) {
        let temp_dir = tempdir().expect("temp dir");
        let workflow_dir = temp_dir.path().join(".flowdex/workflows");
        fs::create_dir_all(&workflow_dir).expect("workflow directory");
        fs::write(workflow_dir.join("hello.js"), source).expect("workflow source");
        let loader = WorkflowLoader::new(
            AbsolutePathBuf::from_absolute_path(temp_dir.path()).expect("absolute temp dir"),
        );
        (temp_dir, loader)
    }

    #[test]
    fn loads_source_with_frozen_json_bootstrap() {
        let (_temp_dir, loader) = loader_with_workflow("emit('hello');");
        let loaded = loader
            .load(
                Path::new(".flowdex/workflows/hello.js"),
                Some(&json!({"quote": "line\nnext"})),
            )
            .expect("workflow should load");
        assert!(loaded.source.starts_with("const flowdex = Object.freeze({"));
        assert!(loaded.source.contains("spawnAgent: async"));
        assert!(loaded.source.contains("sendMessage: async"));
        assert!(loaded.source.contains("waitAgent: async"));
        assert!(loaded.source.contains("resumeAgent: async"));
        assert!(loaded.source.contains("progress: async"));
        assert!(loaded.source.contains(r#"input: {"quote":"line\nnext"}"#));
        assert!(
            loaded
                .source
                .contains(r#"workflowPath: ".flowdex/workflows/hello.js""#)
        );
        assert!(loaded.source.ends_with("emit('hello');"));
    }

    #[test]
    fn rejects_invalid_workflow_paths() {
        let (_temp_dir, loader) = loader_with_workflow("ok");
        let cases = [
            ("../outside.js", "invalid"),
            (".flowdex/workflows/../outside.js", "invalid"),
            (".flowdex/workflows/hello.txt", "extension"),
            (".flowdex/workflows/missing.js", "not found"),
            ("workflows/hello.js", "outside"),
        ];
        for (path, expected) in cases {
            let error = loader.load(Path::new(path), None).expect_err(path);
            let message = error.to_string();
            assert!(message.contains(expected), "{path}: {message}");
        }

        fs::create_dir(_temp_dir.path().join(".flowdex/workflows/directory.js"))
            .expect("directory workflow");
        assert!(
            loader
                .load(Path::new(".flowdex/workflows/directory.js"), None)
                .is_err()
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                _temp_dir.path().join("outside.js"),
                _temp_dir.path().join(".flowdex/workflows/link.js"),
            )
            .expect("symlink");
            fs::write(_temp_dir.path().join("outside.js"), "secret").expect("outside source");
            assert!(matches!(
                loader.load(Path::new(".flowdex/workflows/link.js"), None),
                Err(WorkflowLoadError::OutsideWorkflowRoot)
            ));
        }
    }
}
