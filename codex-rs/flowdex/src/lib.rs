pub mod config;
pub mod store;
pub mod workflow;

pub use config::DEFAULT_COMPACTION_REMINDER_THRESHOLD_TOKENS;
pub use config::FlowdexConfig;
pub use config::FlowdexConfigError;
pub use config::load_config;
pub use store::FlowdexStore;
pub use store::FlowdexStoreError;
pub use store::IntegrationResult;
pub use store::PhaseMetadata;
pub use store::PhaseState;
pub use store::RunMetadata;
pub use store::RunInfo;
pub use store::RunState;
pub use store::ScheduledTask;
pub use store::ScheduledTaskDetails;
pub use store::SchedulerTaskState;
pub use store::TaskCommit;
pub use store::TaskDeclaration;
pub use store::TaskOperation;
pub use store::TaskRecord;
pub use workflow::AgentDefinition;
pub use workflow::PhaseDefinition;
pub use workflow::TaskDefinition;
pub use workflow::WorkflowDefinition;
pub use workflow::WorkflowValidationError;
pub use workflow::write_scope_conflicts;

use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use thiserror::Error;

const WORKFLOW_DIRECTORY: [&str; 2] = [".flowdex", "workflows"];
const RESUME_AGENT_BOOTSTRAP: &str = r#"  resumeAgent: async (agentId, instructions, options = {}) => {
    if (options === null || typeof options !== "object" || Array.isArray(options)) {
      throw new TypeError("resumeAgent options must be an object");
    }
    const unknownOptions = Object.keys(options).filter((key) => key !== "contextMode");
    if (unknownOptions.length > 0) {
      throw new TypeError(`resumeAgent unknown option: ${unknownOptions[0]}`);
    }
    return tools.flowdex_resume_agent({ agent_id: agentId, instructions, options: { context_mode: options.contextMode } });
  },
"#;
const TASK_BOOTSTRAP: &str = r#"  createTask: async (declaration) => {
    if (declaration === null || typeof declaration !== "object" || Array.isArray(declaration)) {
      throw new TypeError("createTask declaration must be an object");
    }
    const declarationKeys = new Set(["name", "instructions", "readScope", "writeScope", "verification"]);
    const unknownDeclarationKeys = Object.keys(declaration).filter((key) => !declarationKeys.has(key));
    if (unknownDeclarationKeys.length > 0) {
      throw new TypeError(`createTask unknown field: ${unknownDeclarationKeys[0]}`);
    }
    const created = await tools.flowdex_create_task({
      name: declaration.name,
      instructions: declaration.instructions,
      read_scope: declaration.readScope,
      write_scope: declaration.writeScope,
      verification: declaration.verification,
      workflow_path: flowdex.workflowPath,
    });
    const task = {
      id: created.taskId,
      runAgent: async (agentSpec) => {
        if (agentSpec === null || typeof agentSpec !== "object" || Array.isArray(agentSpec)) {
          throw new TypeError("runAgent agentSpec must be an object");
        }
        const agentKeys = new Set(["name", "instructions", "profile", "model", "reasoningEffort"]);
        const unknownAgentKeys = Object.keys(agentSpec).filter((key) => !agentKeys.has(key));
        if (unknownAgentKeys.length > 0) {
          throw new TypeError(`runAgent unknown field: ${unknownAgentKeys[0]}`);
        }
        return tools.flowdex_task_run_agent({
          task_id: created.taskId,
          agent: {
            name: agentSpec.name,
            instructions: agentSpec.instructions,
            profile: agentSpec.profile,
            model: agentSpec.model,
            reasoning_effort: agentSpec.reasoningEffort,
          },
        });
      },
      verify: async () => tools.flowdex_task_verify({ task_id: created.taskId }),
      integrate: async () => tools.flowdex_task_integrate({ task_id: created.taskId }),
    };
    return Object.freeze(task);
  },
"#;
const START_RUN_BOOTSTRAP: &str = r#"  startRun: async (definition) => {
    const isPlainObject = (value) =>
      value !== null && typeof value === "object" && !Array.isArray(value) &&
      Object.getPrototypeOf(value) === Object.prototype;
    const requireObject = (value, label) => {
      if (!isPlainObject(value)) throw new TypeError(`${label} must be a plain object`);
      return value;
    };
    const requireString = (value, label) => {
      if (typeof value !== "string" || value.trim().length === 0) {
        throw new TypeError(`${label} must be a non-empty string`);
      }
      return value;
    };
    const requireKeys = (value, allowed, label) => {
      const unknown = Object.keys(value).find((key) => !allowed.has(key));
      if (unknown !== undefined) throw new TypeError(`${label} unknown field: ${unknown}`);
    };
    const commandArray = (value, label) => {
      if (value === undefined) return [];
      if (!Array.isArray(value)) throw new TypeError(`${label} must be an array`);
      return value.map((command, index) => requireString(command, `${label}[${index}]`));
    };
    const selector = (value, label) =>
      value === undefined ? undefined : requireString(value, label);
    const taskDefinition = (task, knownAgents, label = "task") => {
      requireObject(task, `${label} definition`);
      requireKeys(task, new Set(["name", "agent", "instructions", "dependencies", "readScope", "writeScope", "verification"]), label);
      const name = requireString(task.name, `${label}.name`);
      const agent = requireString(task.agent, `${label}.agent`);
      if (!knownAgents.has(agent)) throw new TypeError(`${label}.agent is unknown`);
      const dependencies = task.dependencies === undefined ? [] : task.dependencies;
      if (!Array.isArray(dependencies)) throw new TypeError(`${label}.dependencies must be an array`);
      return {
        name,
        agent,
        instructions: requireString(task.instructions, `${label}.instructions`),
        dependencies: dependencies.map((dependency, index) =>
          requireString(dependency, `${label}.dependencies[${index}]`)),
        read_scope: commandArray(task.readScope, `${label}.readScope`),
        write_scope: commandArray(task.writeScope, `${label}.writeScope`),
        verification: commandArray(task.verification, `${label}.verification`),
      };
    };

    requireObject(definition, "startRun definition");
    requireKeys(definition, new Set(["name", "agents", "phases", "verification"]), "startRun");
    const runName = requireString(definition.name, "startRun.name");
    const agents = requireObject(definition.agents, "startRun.agents");
    const agentNames = new Set(Object.keys(agents));
    if (agentNames.size === 0) throw new TypeError("startRun.agents must not be empty");
    const normalizedAgents = {};
    for (const agentName of agentNames) {
      requireString(agentName, "startRun agent name");
      const agent = requireObject(agents[agentName], `startRun.agents.${agentName}`);
      requireKeys(agent, new Set(["profile", "model", "reasoningEffort"]), `startRun.agents.${agentName}`);
      const profile = selector(agent.profile, `startRun.agents.${agentName}.profile`);
      const model = selector(agent.model, `startRun.agents.${agentName}.model`);
      const reasoningEffort = selector(agent.reasoningEffort, `startRun.agents.${agentName}.reasoningEffort`);
      if (profile === undefined && model === undefined && reasoningEffort === undefined) {
        throw new TypeError(`startRun.agents.${agentName} needs a profile, model, or reasoningEffort`);
      }
      normalizedAgents[agentName] = {};
      if (profile !== undefined) normalizedAgents[agentName].profile = profile;
      if (model !== undefined) normalizedAgents[agentName].model = model;
      if (reasoningEffort !== undefined) normalizedAgents[agentName].reasoning_effort = reasoningEffort;
    }

    if (!Array.isArray(definition.phases) || definition.phases.length === 0) {
      throw new TypeError("startRun.phases must be a non-empty array");
    }
    const phaseNames = new Set();
    const phases = definition.phases.map((phase, phaseIndex) => {
      requireObject(phase, `startRun.phases[${phaseIndex}]`);
      requireKeys(phase, new Set(["name", "instructions", "tasks", "open", "verification"]), `startRun.phases[${phaseIndex}]`);
      const name = requireString(phase.name, `startRun.phases[${phaseIndex}].name`);
      if (phaseNames.has(name)) throw new TypeError(`duplicate phase name: ${name}`);
      phaseNames.add(name);
      const open = phase.open === undefined ? false : phase.open;
      if (typeof open !== "boolean") throw new TypeError(`startRun.phases[${phaseIndex}].open must be boolean`);
      if (!Array.isArray(phase.tasks)) throw new TypeError(`startRun.phases[${phaseIndex}].tasks must be an array`);
      if (!open && phase.tasks.length === 0) throw new TypeError(`closed phase ${name} must have tasks`);
      const names = new Set();
      const tasks = phase.tasks.map((task, taskIndex) => {
        const normalized = taskDefinition(task, agentNames, `startRun.phases[${phaseIndex}].tasks[${taskIndex}]`);
        if (names.has(normalized.name)) throw new TypeError(`duplicate task name in phase ${name}: ${normalized.name}`);
        names.add(normalized.name);
        return normalized;
      });
      for (const task of tasks) {
        for (const dependency of task.dependencies) {
          if (!names.has(dependency)) throw new TypeError(`missing dependency in phase ${name}: ${dependency}`);
        }
      }
      const visiting = new Set();
      const visited = new Set();
      const visit = (taskName) => {
        if (visiting.has(taskName)) throw new TypeError(`dependency cycle in phase ${name}`);
        if (visited.has(taskName)) return;
        visiting.add(taskName);
        const task = tasks.find((candidate) => candidate.name === taskName);
        for (const dependency of task.dependencies) visit(dependency);
        visiting.delete(taskName);
        visited.add(taskName);
      };
      for (const task of tasks) visit(task.name);
      return {
        name,
        instructions: requireString(phase.instructions, `startRun.phases[${phaseIndex}].instructions`),
        tasks,
        open,
        verification: commandArray(phase.verification, `startRun.phases[${phaseIndex}].verification`),
      };
    });
    const normalized = {
      name: runName,
      agents: normalizedAgents,
      phases,
      verification: commandArray(definition.verification, "startRun.verification"),
    };
    const created = await tools.flowdex_start_run({
      definition: normalized,
      workflow_path: flowdex.workflowPath,
    });
    const id = created.runId;
    const handle = {
      id,
      queueTask: async (phaseName, task) => {
        requireString(phaseName, "queueTask.phase");
        const normalizedTask = taskDefinition(task, agentNames, "queueTask.task");
        const queued = await tools.flowdex_queue_task({
          run_id: id,
          phase: phaseName,
          task: normalizedTask,
        });
        return { taskId: queued.taskId };
      },
      sealPhase: async (phaseName) => {
        requireString(phaseName, "sealPhase.phase");
        await tools.flowdex_seal_phase({ run_id: id, phase: phaseName });
      },
      wait: async () => {
        const result = await tools.flowdex_wait_run({ run_id: id });
        return { runId: result.runId, status: result.status };
      },
    };
    return Object.freeze(handle);
  },
"#;

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
                "const flowdex = Object.freeze({{\n  input: {input},\n  workflowPath: {workflow_path},\n  spawnAgent: async (spec) => tools.flowdex_spawn_agent({{\n    name: spec.name,\n    instructions: spec.instructions,\n    profile: spec.profile,\n    model: spec.model,\n    reasoning_effort: spec.reasoningEffort,\n  }}),\n  sendMessage: async (agentId, message, options = {{}}) => tools.flowdex_send_message({{\n    agent_id: agentId,\n    message,\n    delivery: options.delivery ?? \"queue\",\n  }}),\n  waitAgent: async (agentId) => tools.flowdex_wait_agent({{ agent_id: agentId }}),\n{RESUME_AGENT_BOOTSTRAP}{TASK_BOOTSTRAP}{START_RUN_BOOTSTRAP}  verify: async (commands, options = {{}}) => tools.flowdex_verify({{\n    commands,\n    workdir: options.workdir,\n    timeout_ms: options.timeoutMs,\n  }}),\n}});\n\n{source}"
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
        assert!(loaded.source.contains("startRun: async"));
        assert!(loaded.source.contains("Object.freeze(handle)"));
        assert!(loaded.source.contains("tools.flowdex_start_run"));
        assert!(loaded.source.contains("tools.flowdex_queue_task"));
        assert!(loaded.source.contains("tools.flowdex_seal_phase"));
        assert!(loaded.source.contains("tools.flowdex_wait_run"));
        assert!(loaded.source.contains("reasoning_effort"));
        assert!(loaded.source.contains("write_scope"));
        assert!(loaded.source.contains("options must be an object"));
        assert!(loaded.source.contains("key !== \"contextMode\""));
        assert!(loaded.source.contains("createTask unknown field"));
        assert!(loaded.source.contains("runAgent unknown field"));
        assert!(
            loaded
                .source
                .contains("reasoning_effort: agentSpec.reasoningEffort")
        );
        assert!(!loaded.source.contains("progress: async"));
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
