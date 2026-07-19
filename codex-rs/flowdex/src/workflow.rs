use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentDefinition {
    pub profile: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub name: String,
    pub agents: BTreeMap<String, AgentDefinition>,
    #[serde(default)]
    pub context_packs: BTreeMap<String, ContextPackDefinition>,
    #[serde(default)]
    pub verification: Vec<String>,
    pub phases: Vec<PhaseDefinition>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextPackDefinition {
    pub agent: String,
    pub instructions: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhaseDefinition {
    pub name: String,
    pub instructions: String,
    pub tasks: Vec<TaskDefinition>,
    #[serde(default)]
    pub open: bool,
    #[serde(default)]
    pub verification: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskDefinition {
    pub name: String,
    pub agent: String,
    pub instructions: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub read_scope: Vec<String>,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub context: Vec<String>,
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum WorkflowValidationError {
    #[error("{0} must be a non-empty string")]
    EmptyString(&'static str),
    #[error("workflow must define at least one agent")]
    NoAgents,
    #[error("workflow must define at least one phase")]
    NoPhases,
    #[error("agent {0} must specify profile, model, or reasoning_effort")]
    AgentWithoutSelector(String),
    #[error("context pack {0} must define an agent and instructions")]
    InvalidContextPack(String),
    #[error("context pack {pack} references unknown agent {agent}")]
    UnknownContextAgent { pack: String, agent: String },
    #[error("task {task} references unknown context pack {pack}")]
    UnknownContextPack { task: String, pack: String },
    #[error("task {task} references duplicate context pack {pack}")]
    DuplicateTaskContext { task: String, pack: String },
    #[error("duplicate agent: {0}")]
    DuplicateAgent(String),
    #[error("duplicate phase: {0}")]
    DuplicatePhase(String),
    #[error("phase {0} must contain a task unless it is open")]
    EmptyClosedPhase(String),
    #[error("duplicate task in phase {phase}: {task}")]
    DuplicateTask { phase: String, task: String },
    #[error("task {task} references unknown agent {agent}")]
    UnknownAgent { task: String, agent: String },
    #[error("task {task} in phase {phase} references unknown dependency {dependency}")]
    UnknownDependency {
        phase: String,
        task: String,
        dependency: String,
    },
    #[error("dependency cycle in phase {0}")]
    DependencyCycle(String),
    #[error("phase is not open: {0}")]
    PhaseNotOpen(String),
}

/// Returns whether two advisory write scopes have equal or ancestor roots.
pub fn write_scope_conflicts(left: &[String], right: &[String]) -> bool {
    left.iter().map(normalized_scope_root).any(|left| {
        right.iter().map(normalized_scope_root).any(|right| {
            left == right
                || left.starts_with(&(right.clone() + "/"))
                || right.starts_with(&(left.clone() + "/"))
        })
    })
}

fn normalized_scope_root(scope: &String) -> String {
    let mut value = scope.trim().replace('\\', "/");
    while value.starts_with("./") {
        value = value[2..].to_string();
    }
    while value.ends_with('/') {
        value.pop();
    }
    if let Some(index) = value.find("/**") {
        value.truncate(index);
    }
    value
}

impl WorkflowDefinition {
    pub fn validate(&self) -> Result<(), WorkflowValidationError> {
        non_empty("workflow name", &self.name)?;
        if self.agents.is_empty() {
            return Err(WorkflowValidationError::NoAgents);
        }
        if self.phases.is_empty() {
            return Err(WorkflowValidationError::NoPhases);
        }
        for (name, agent) in &self.agents {
            non_empty("agent name", name)?;
            if agent.profile.is_none() && agent.model.is_none() && agent.reasoning_effort.is_none()
            {
                return Err(WorkflowValidationError::AgentWithoutSelector(name.clone()));
            }
            for value in [&agent.profile, &agent.model, &agent.reasoning_effort]
                .into_iter()
                .flatten()
            {
                non_empty("agent selector", value)?;
            }
        }
        validate_commands(&self.verification)?;
        for (name, pack) in &self.context_packs {
            non_empty("context pack name", name)?;
            non_empty("context pack agent", &pack.agent)
                .map_err(|_| WorkflowValidationError::InvalidContextPack(name.clone()))?;
            non_empty("context pack instructions", &pack.instructions)
                .map_err(|_| WorkflowValidationError::InvalidContextPack(name.clone()))?;
            if !self.agents.contains_key(&pack.agent) {
                return Err(WorkflowValidationError::UnknownContextAgent {
                    pack: name.clone(),
                    agent: pack.agent.clone(),
                });
            }
        }
        let mut phases = BTreeSet::new();
        for phase in &self.phases {
            non_empty("phase name", &phase.name)?;
            if !phases.insert(&phase.name) {
                return Err(WorkflowValidationError::DuplicatePhase(phase.name.clone()));
            }
            non_empty("phase instructions", &phase.instructions)?;
            if phase.tasks.is_empty() && !phase.open {
                return Err(WorkflowValidationError::EmptyClosedPhase(
                    phase.name.clone(),
                ));
            }
            validate_commands(&phase.verification)?;
            self.validate_phase_tasks(phase)?;
        }
        Ok(())
    }

    fn validate_phase_tasks(&self, phase: &PhaseDefinition) -> Result<(), WorkflowValidationError> {
        let mut names = BTreeSet::new();
        for task in &phase.tasks {
            validate_task_shape(task)?;
            if !names.insert(&task.name) {
                return Err(WorkflowValidationError::DuplicateTask {
                    phase: phase.name.clone(),
                    task: task.name.clone(),
                });
            }
            if !self.agents.contains_key(&task.agent) {
                return Err(WorkflowValidationError::UnknownAgent {
                    task: task.name.clone(),
                    agent: task.agent.clone(),
                });
            }
            for dependency in &task.dependencies {
                if !names.contains(dependency)
                    && !phase
                        .tasks
                        .iter()
                        .any(|candidate| candidate.name == *dependency)
                {
                    return Err(WorkflowValidationError::UnknownDependency {
                        phase: phase.name.clone(),
                        task: task.name.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
            self.validate_task_context(task)?;
        }
        detect_cycle(&phase.name, &phase.tasks)
    }

    pub fn validate_dynamic_task(
        &self,
        phase_name: &str,
        task: &TaskDefinition,
    ) -> Result<(), WorkflowValidationError> {
        let phase = self
            .phases
            .iter()
            .find(|phase| phase.name == phase_name)
            .ok_or_else(|| WorkflowValidationError::UnknownDependency {
                phase: phase_name.to_string(),
                task: task.name.clone(),
                dependency: phase_name.to_string(),
            })?;
        if !phase.open {
            return Err(WorkflowValidationError::PhaseNotOpen(
                phase_name.to_string(),
            ));
        }
        validate_task_shape(task)?;
        if phase
            .tasks
            .iter()
            .any(|existing| existing.name == task.name)
        {
            return Err(WorkflowValidationError::DuplicateTask {
                phase: phase_name.to_string(),
                task: task.name.clone(),
            });
        }
        if !self.agents.contains_key(&task.agent) {
            return Err(WorkflowValidationError::UnknownAgent {
                task: task.name.clone(),
                agent: task.agent.clone(),
            });
        }
        self.validate_task_context(task)?;
        for dependency in &task.dependencies {
            if !phase
                .tasks
                .iter()
                .any(|existing| existing.name == *dependency)
            {
                return Err(WorkflowValidationError::UnknownDependency {
                    phase: phase_name.to_string(),
                    task: task.name.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
        let mut tasks = phase.tasks.clone();
        tasks.push(task.clone());
        detect_cycle(phase_name, &tasks)
    }

    fn validate_task_context(&self, task: &TaskDefinition) -> Result<(), WorkflowValidationError> {
        let mut names = BTreeSet::new();
        for pack in &task.context {
            if !names.insert(pack) {
                return Err(WorkflowValidationError::DuplicateTaskContext {
                    task: task.name.clone(),
                    pack: pack.clone(),
                });
            }
            if !self.context_packs.contains_key(pack) {
                return Err(WorkflowValidationError::UnknownContextPack {
                    task: task.name.clone(),
                    pack: pack.clone(),
                });
            }
        }
        Ok(())
    }
}

fn validate_task_shape(task: &TaskDefinition) -> Result<(), WorkflowValidationError> {
    non_empty("task name", &task.name)?;
    non_empty("task agent", &task.agent)?;
    non_empty("task instructions", &task.instructions)?;
    validate_commands(&task.read_scope)?;
    validate_commands(&task.write_scope)?;
    validate_commands(&task.verification)
}

pub fn validate_task_definition(task: &TaskDefinition) -> Result<(), WorkflowValidationError> {
    validate_task_shape(task)
}

fn validate_commands(values: &[String]) -> Result<(), WorkflowValidationError> {
    for value in values {
        non_empty("command or scope", value)?;
    }
    Ok(())
}

fn non_empty(label: &'static str, value: &str) -> Result<(), WorkflowValidationError> {
    if value.trim().is_empty() {
        Err(WorkflowValidationError::EmptyString(label))
    } else {
        Ok(())
    }
}

fn detect_cycle(phase: &str, tasks: &[TaskDefinition]) -> Result<(), WorkflowValidationError> {
    let names: BTreeSet<&str> = tasks.iter().map(|task| task.name.as_str()).collect();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for task in tasks {
        if visit(
            task.name.as_str(),
            tasks,
            &names,
            &mut visiting,
            &mut visited,
        ) {
            return Err(WorkflowValidationError::DependencyCycle(phase.to_string()));
        }
    }
    Ok(())
}

fn visit<'a>(
    name: &'a str,
    tasks: &'a [TaskDefinition],
    names: &BTreeSet<&'a str>,
    visiting: &mut BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
) -> bool {
    if visiting.contains(name) {
        return true;
    }
    if !visited.insert(name) {
        return false;
    }
    visiting.insert(name);
    if let Some(task) = tasks.iter().find(|task| task.name == name) {
        for dep in &task.dependencies {
            if names.contains(dep.as_str()) && visit(dep, tasks, names, visiting, visited) {
                return true;
            }
        }
    }
    visiting.remove(name);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task(name: &str, deps: &[&str]) -> TaskDefinition {
        TaskDefinition {
            name: name.into(),
            agent: "a".into(),
            instructions: "do".into(),
            dependencies: deps.iter().map(|v| (*v).into()).collect(),
            read_scope: vec![],
            write_scope: vec![],
            verification: vec![],
            context: vec![],
        }
    }
    fn workflow(tasks: Vec<TaskDefinition>) -> WorkflowDefinition {
        WorkflowDefinition {
            name: "run".into(),
            agents: [(
                "a".into(),
                AgentDefinition {
                    profile: Some("worker".into()),
                    model: None,
                    reasoning_effort: None,
                },
            )]
            .into_iter()
            .collect(),
            verification: vec![],
            context_packs: BTreeMap::new(),
            phases: vec![PhaseDefinition {
                name: "phase".into(),
                instructions: "phase".into(),
                tasks,
                open: false,
                verification: vec![],
            }],
        }
    }
    #[test]
    fn rejects_cycle() {
        let error = workflow(vec![task("a", &["b"]), task("b", &["a"])])
            .validate()
            .unwrap_err();
        assert!(matches!(error, WorkflowValidationError::DependencyCycle(_)));
    }
    #[test]
    fn rejects_missing_dependency_atomically() {
        let error = workflow(vec![task("a", &["missing"])])
            .validate()
            .unwrap_err();
        assert!(matches!(
            error,
            WorkflowValidationError::UnknownDependency { .. }
        ));
    }
    #[test]
    fn dynamic_requires_existing_open_phase() {
        let mut definition = workflow(vec![]);
        definition.phases[0].open = true;
        assert!(
            definition
                .validate_dynamic_task("phase", &task("new", &[]))
                .is_ok()
        );
    }
    #[test]
    fn scope_conflict_uses_normalized_roots() {
        assert!(write_scope_conflicts(
            &["./src/**".into()],
            &["src/parser/**".into()]
        ));
        assert!(!write_scope_conflicts(
            &["docs/**".into()],
            &["src/**".into()]
        ));
    }

    #[test]
    fn rejects_unknown_agent_and_invalid_command() {
        let mut definition = workflow(vec![task("a", &[])]);
        definition.phases[0].tasks[0].agent = "missing".into();
        assert!(matches!(
            definition.validate(),
            Err(WorkflowValidationError::UnknownAgent { .. })
        ));

        let mut definition = workflow(vec![task("a", &[])]);
        definition.phases[0].tasks[0].verification = vec!["  ".into()];
        assert!(matches!(
            definition.validate(),
            Err(WorkflowValidationError::EmptyString("command or scope"))
        ));
    }
}
