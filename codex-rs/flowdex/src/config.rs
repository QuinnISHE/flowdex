use codex_config::McpServerConfig;
use codex_config::config_toml::ToolsToml;
use codex_protocol::config_types::WebSearchMode;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use thiserror::Error;

pub const DEFAULT_COMPACTION_REMINDER_THRESHOLD_TOKENS: i64 = 185_000;
pub const DEFAULT_AST_GREP_CANDIDATE_THRESHOLD: i64 = 3;
pub const DEFAULT_VERIFICATION_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_SUBAGENT_EXCLUDED_SKILL: &str = "run-flowdex-workflows";

/// Multi-agent backend version selected by Flowdex configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowdexMultiAgentVersion {
    V1,
    V2,
}

/// Base coding prompt selected by Flowdex configuration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowdexSystemPromptMode {
    Claude,
    #[default]
    Codex,
    Pi,
}

/// Resolved Flowdex settings used by a session.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowdexConfig {
    pub compaction_reminder_threshold_tokens: i64,
    pub verification_timeout_ms: u64,
    pub ast_grep_candidate_threshold: i64,
    pub ast_grep_always_run: Vec<String>,
    pub tool_profiles: BTreeMap<String, ToolProfileConfig>,
    pub multi_agent_version: Option<FlowdexMultiAgentVersion>,
    pub system_prompt_mode: FlowdexSystemPromptMode,
    pub subagent_excluded_tools: Vec<String>,
    pub subagent_excluded_skills: Vec<String>,
    /// Resolved only on a Flowdex child config after applying its tool profile.
    pub active_agent_excluded_tools: Vec<String>,
}

/// Tool-related configuration that can be selected by a Flowdex agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolProfileConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_search: Option<WebSearchMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsToml>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_skills: Vec<String>,
}

/// Loads global settings and, when eligible, a trusted repository override.
///
/// The caller is responsible for the trust decision. Pass the repository root
/// only when its `.flowdex/config.toml` is allowed to participate.
pub fn load_config(
    codex_home: &Path,
    trusted_repository_root: Option<&Path>,
) -> Result<FlowdexConfig, FlowdexConfigError> {
    let global_path = codex_home.join("flowdex.toml");
    let mut threshold = DEFAULT_COMPACTION_REMINDER_THRESHOLD_TOKENS;
    let mut verification_timeout_ms = DEFAULT_VERIFICATION_TIMEOUT_MS;
    let mut ast_grep_candidate_threshold = DEFAULT_AST_GREP_CANDIDATE_THRESHOLD;
    let mut ast_grep_always_run = Vec::new();
    let mut tool_profiles = BTreeMap::new();
    let mut multi_agent_version = Some(FlowdexMultiAgentVersion::V1);
    let mut system_prompt_mode = FlowdexSystemPromptMode::Codex;
    let mut subagent_excluded_tools = Vec::new();
    let mut subagent_excluded_skills = vec![DEFAULT_SUBAGENT_EXCLUDED_SKILL.to_string()];

    if let Some(config) = read_partial(&global_path)? {
        if let Some(value) = config.compaction_reminder_threshold_tokens {
            validate_threshold(value, &global_path)?;
            threshold = value;
        }
        if let Some(value) = config.verification_timeout_ms {
            validate_verification_timeout(value, &global_path)?;
            verification_timeout_ms = value;
        }
        if let Some(value) = config.ast_grep_candidate_threshold {
            validate_candidate_threshold(value, &global_path)?;
            ast_grep_candidate_threshold = value;
        }
        if let Some(value) = config.ast_grep_always_run {
            validate_rule_ids(&value, &global_path)?;
            ast_grep_always_run = value;
        }
        validate_tool_profiles(&config.tool_profiles, &global_path)?;
        tool_profiles.extend(config.tool_profiles);
        if let Some(value) = config.multi_agent_version {
            multi_agent_version = Some(value);
        }
        if let Some(value) = config.system_prompt_mode {
            system_prompt_mode = value;
        }
        if let Some(value) = config.subagent_excluded_tools {
            validate_names(&value, &global_path, "subagent_excluded_tools")?;
            subagent_excluded_tools = value;
        }
        if let Some(value) = config.subagent_excluded_skills {
            validate_names(&value, &global_path, "subagent_excluded_skills")?;
            subagent_excluded_skills = value;
        }
    }

    if let Some(repository_root) = trusted_repository_root {
        let repository_path = repository_root.join(".flowdex").join("config.toml");
        if let Some(config) = read_partial(&repository_path)? {
            if let Some(value) = config.compaction_reminder_threshold_tokens {
                validate_threshold(value, &repository_path)?;
                threshold = value;
            }
            if let Some(value) = config.verification_timeout_ms {
                validate_verification_timeout(value, &repository_path)?;
                verification_timeout_ms = value;
            }
            if let Some(value) = config.ast_grep_candidate_threshold {
                validate_candidate_threshold(value, &repository_path)?;
                ast_grep_candidate_threshold = value;
            }
            if let Some(value) = config.ast_grep_always_run {
                validate_rule_ids(&value, &repository_path)?;
                ast_grep_always_run = value;
            }
            validate_tool_profiles(&config.tool_profiles, &repository_path)?;
            tool_profiles.extend(config.tool_profiles);
            if let Some(value) = config.multi_agent_version {
                multi_agent_version = Some(value);
            }
            if let Some(value) = config.system_prompt_mode {
                system_prompt_mode = value;
            }
            if let Some(value) = config.subagent_excluded_tools {
                validate_names(&value, &repository_path, "subagent_excluded_tools")?;
                subagent_excluded_tools = value;
            }
            if let Some(value) = config.subagent_excluded_skills {
                validate_names(&value, &repository_path, "subagent_excluded_skills")?;
                subagent_excluded_skills = value;
            }
        }
    }

    Ok(FlowdexConfig {
        compaction_reminder_threshold_tokens: threshold,
        verification_timeout_ms,
        ast_grep_candidate_threshold,
        ast_grep_always_run,
        tool_profiles,
        multi_agent_version,
        system_prompt_mode,
        subagent_excluded_tools,
        subagent_excluded_skills,
        active_agent_excluded_tools: Vec::new(),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialFlowdexConfig {
    compaction_reminder_threshold_tokens: Option<i64>,
    verification_timeout_ms: Option<u64>,
    ast_grep_candidate_threshold: Option<i64>,
    ast_grep_always_run: Option<Vec<String>>,
    multi_agent_version: Option<FlowdexMultiAgentVersion>,
    system_prompt_mode: Option<FlowdexSystemPromptMode>,
    subagent_excluded_tools: Option<Vec<String>>,
    subagent_excluded_skills: Option<Vec<String>>,
    #[serde(default)]
    tool_profiles: BTreeMap<String, ToolProfileConfig>,
}

fn read_partial(path: &Path) -> Result<Option<PartialFlowdexConfig>, FlowdexConfigError> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(FlowdexConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    toml::from_str(&source)
        .map(Some)
        .map_err(|source| FlowdexConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
}

fn validate_threshold(value: i64, path: &Path) -> Result<(), FlowdexConfigError> {
    if value <= 0 {
        return Err(FlowdexConfigError::InvalidThreshold {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn validate_candidate_threshold(value: i64, path: &Path) -> Result<(), FlowdexConfigError> {
    if value <= 0 {
        return Err(FlowdexConfigError::InvalidCandidateThreshold {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn validate_verification_timeout(value: u64, path: &Path) -> Result<(), FlowdexConfigError> {
    if value == 0 {
        return Err(FlowdexConfigError::InvalidVerificationTimeout {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn validate_rule_ids(values: &[String], path: &Path) -> Result<(), FlowdexConfigError> {
    let mut seen = std::collections::HashSet::new();
    for value in values {
        if value.trim().is_empty() {
            return Err(FlowdexConfigError::InvalidAstGrepRuleId {
                path: path.to_path_buf(),
                id: value.clone(),
            });
        }
        if !seen.insert(value) {
            return Err(FlowdexConfigError::DuplicateAstGrepRuleId {
                path: path.to_path_buf(),
                id: value.clone(),
            });
        }
    }
    Ok(())
}

fn validate_names(
    values: &[String],
    path: &Path,
    field: &'static str,
) -> Result<(), FlowdexConfigError> {
    let mut seen = std::collections::HashSet::new();
    for value in values {
        if value.trim().is_empty() || !seen.insert(value) {
            return Err(FlowdexConfigError::InvalidExclusion {
                path: path.to_path_buf(),
                field,
                value: value.clone(),
            });
        }
    }
    Ok(())
}

fn validate_tool_profiles(
    profiles: &BTreeMap<String, ToolProfileConfig>,
    path: &Path,
) -> Result<(), FlowdexConfigError> {
    for (name, profile) in profiles {
        validate_names(
            &profile.excluded_tools,
            path,
            "tool_profiles.<name>.excluded_tools",
        )?;
        validate_names(
            &profile.excluded_skills,
            path,
            "tool_profiles.<name>.excluded_skills",
        )?;
        if name.trim().is_empty() {
            return Err(FlowdexConfigError::InvalidExclusion {
                path: path.to_path_buf(),
                field: "tool_profiles",
                value: name.clone(),
            });
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum FlowdexConfigError {
    #[error("unable to read Flowdex config at {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("invalid Flowdex config at {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error(
        "invalid compaction_reminder_threshold_tokens in Flowdex config at {path}: value must be positive"
    )]
    InvalidThreshold { path: PathBuf },

    #[error("invalid verification_timeout_ms in Flowdex config at {path}: value must be positive")]
    InvalidVerificationTimeout { path: PathBuf },

    #[error(
        "invalid ast_grep_candidate_threshold in Flowdex config at {path}: value must be positive"
    )]
    InvalidCandidateThreshold { path: PathBuf },

    #[error(
        "invalid ast_grep_always_run rule id `{id}` in Flowdex config at {path}: id must be non-empty"
    )]
    InvalidAstGrepRuleId { path: PathBuf, id: String },

    #[error("duplicate ast_grep_always_run rule id `{id}` in Flowdex config at {path}")]
    DuplicateAstGrepRuleId { path: PathBuf, id: String },

    #[error(
        "invalid {field} entry `{value}` in Flowdex config at {path}: values must be non-empty and unique"
    )]
    InvalidExclusion {
        path: PathBuf,
        field: &'static str,
        value: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn config(codex_home: &Path, repository_root: Option<&Path>) -> FlowdexConfig {
        load_config(codex_home, repository_root).expect("Flowdex config should load")
    }

    #[test]
    fn resolves_default_global_and_repository_values() {
        let temp = tempdir().expect("temp dir");
        let repository_root = temp.path().join("repo");
        fs::create_dir_all(repository_root.join(".flowdex")).expect("repository config dir");

        assert_eq!(
            config(temp.path(), Some(&repository_root)).compaction_reminder_threshold_tokens,
            DEFAULT_COMPACTION_REMINDER_THRESHOLD_TOKENS
        );
        assert_eq!(
            config(temp.path(), Some(&repository_root)).verification_timeout_ms,
            DEFAULT_VERIFICATION_TIMEOUT_MS
        );
        assert_eq!(
            config(temp.path(), Some(&repository_root)).system_prompt_mode,
            FlowdexSystemPromptMode::Codex
        );
        assert_eq!(
            config(temp.path(), Some(&repository_root)).subagent_excluded_skills,
            [DEFAULT_SUBAGENT_EXCLUDED_SKILL]
        );
        assert!(
            config(temp.path(), Some(&repository_root))
                .subagent_excluded_tools
                .is_empty()
        );

        fs::write(
            temp.path().join("flowdex.toml"),
            "compaction_reminder_threshold_tokens = 120000\nverification_timeout_ms = 600000\n",
        )
        .expect("global config");
        let resolved = config(temp.path(), Some(&repository_root));
        assert_eq!(resolved.compaction_reminder_threshold_tokens, 120000);
        assert_eq!(resolved.verification_timeout_ms, 600000);

        fs::write(
            repository_root.join(".flowdex/config.toml"),
            "compaction_reminder_threshold_tokens = 90000\nverification_timeout_ms = 900000\n",
        )
        .expect("repository config");
        let resolved = config(temp.path(), Some(&repository_root));
        assert_eq!(resolved.compaction_reminder_threshold_tokens, 90000);
        assert_eq!(resolved.verification_timeout_ms, 900000);

        fs::write(
            repository_root.join(".flowdex/config.toml"),
            "ast_grep_always_run = [\"repo-rule\"]\n",
        )
        .expect("repository omission");
        let resolved = config(temp.path(), Some(&repository_root));
        assert_eq!(resolved.compaction_reminder_threshold_tokens, 120000);
        assert_eq!(resolved.verification_timeout_ms, 600000);
        assert_eq!(resolved.ast_grep_always_run, ["repo-rule"]);

        fs::write(
            temp.path().join("flowdex.toml"),
            "ast_grep_always_run = [\"global-rule\"]\n",
        )
        .expect("global rules");
        fs::write(repository_root.join(".flowdex/config.toml"), "# omitted\n")
            .expect("repository omission");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).ast_grep_always_run,
            ["global-rule"]
        );
    }

    #[test]
    fn parses_and_merges_multi_agent_version_with_repository_precedence() {
        let temp = tempdir().expect("temp dir");
        let repository_root = temp.path().join("repo");
        fs::create_dir_all(repository_root.join(".flowdex")).expect("repository config dir");

        assert_eq!(
            config(temp.path(), Some(&repository_root)).multi_agent_version,
            Some(FlowdexMultiAgentVersion::V1)
        );

        fs::write(
            temp.path().join("flowdex.toml"),
            "multi_agent_version = \"v1\"\n",
        )
        .expect("global config");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).multi_agent_version,
            Some(FlowdexMultiAgentVersion::V1)
        );

        fs::write(repository_root.join(".flowdex/config.toml"), "# omitted\n")
            .expect("repository omission");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).multi_agent_version,
            Some(FlowdexMultiAgentVersion::V1)
        );

        fs::write(
            repository_root.join(".flowdex/config.toml"),
            "multi_agent_version = \"v2\"\n",
        )
        .expect("repository config");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).multi_agent_version,
            Some(FlowdexMultiAgentVersion::V2)
        );
    }

    #[test]
    fn rejects_unknown_multi_agent_version_with_path() {
        let temp = tempdir().expect("temp dir");
        let path = temp.path().join("flowdex.toml");
        fs::write(&path, "multi_agent_version = \"v3\"\n").expect("global config");

        let error = load_config(temp.path(), None).expect_err("invalid multi-agent version");
        let message = error.to_string();
        assert!(message.contains("invalid Flowdex config"), "{message}");
        assert!(message.contains("multi_agent_version"), "{message}");
        assert!(message.contains("v3"), "{message}");
        assert!(
            message.contains(path.to_string_lossy().as_ref()),
            "{message}"
        );
    }

    #[test]
    fn resolves_system_prompt_mode_with_repository_precedence() {
        let temp = tempdir().expect("temp dir");
        let repository_root = temp.path().join("repo");
        fs::create_dir_all(repository_root.join(".flowdex")).expect("repository config dir");

        fs::write(
            temp.path().join("flowdex.toml"),
            "system_prompt_mode = \"pi\"\n",
        )
        .expect("global config");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).system_prompt_mode,
            FlowdexSystemPromptMode::Pi
        );

        fs::write(repository_root.join(".flowdex/config.toml"), "# omitted\n")
            .expect("repository omission");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).system_prompt_mode,
            FlowdexSystemPromptMode::Pi
        );

        fs::write(
            repository_root.join(".flowdex/config.toml"),
            "system_prompt_mode = \"codex\"\n",
        )
        .expect("repository config");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).system_prompt_mode,
            FlowdexSystemPromptMode::Codex
        );
    }

    #[test]
    fn rejects_unknown_system_prompt_mode_with_path() {
        let temp = tempdir().expect("temp dir");
        let path = temp.path().join("flowdex.toml");
        fs::write(&path, "system_prompt_mode = \"other\"\n").expect("global config");

        let error = load_config(temp.path(), None).expect_err("invalid prompt mode");
        let message = error.to_string();
        assert!(message.contains("system_prompt_mode"), "{message}");
        assert!(message.contains("other"), "{message}");
        assert!(
            message.contains(path.to_string_lossy().as_ref()),
            "{message}"
        );
    }

    #[test]
    fn rejects_zero_verification_timeout_with_path() {
        let temp = tempdir().expect("temp dir");
        let path = temp.path().join("flowdex.toml");
        fs::write(&path, "verification_timeout_ms = 0\n").expect("global config");

        let error = load_config(temp.path(), None).expect_err("invalid verification timeout");
        let message = error.to_string();
        assert!(message.contains("verification_timeout_ms"), "{message}");
        assert!(
            message.contains(path.to_string_lossy().as_ref()),
            "{message}"
        );
    }

    #[test]
    fn loads_global_profiles_and_replaces_named_profiles_from_repository() {
        let temp = tempdir().expect("temp dir");
        let repository_root = temp.path().join("repo");
        fs::create_dir_all(repository_root.join(".flowdex")).expect("repository config dir");
        fs::write(
            temp.path().join("flowdex.toml"),
            r#"
[tool_profiles.research]
web_search = "live"
excluded_tools = ["apply_patch"]
excluded_skills = ["repo-only-skill"]

[tool_profiles.docs]
web_search = "cached"
"#,
        )
        .expect("global config");
        fs::write(
            repository_root.join(".flowdex/config.toml"),
            r#"
[tool_profiles.research]
web_search = "disabled"
"#,
        )
        .expect("repository config");

        let profiles = config(temp.path(), Some(&repository_root)).tool_profiles;
        assert_eq!(
            profiles["research"].web_search,
            Some(WebSearchMode::Disabled)
        );
        assert!(profiles["research"].excluded_tools.is_empty());
        assert_eq!(profiles["docs"].web_search, Some(WebSearchMode::Cached));
        let serialized = toml::Value::try_from(profiles["docs"].clone()).unwrap();
        assert_eq!(serialized["web_search"].as_str(), Some("cached"));
    }

    #[test]
    fn rejects_malformed_unknown_and_invalid_values_with_paths() {
        let temp = tempdir().expect("temp dir");
        let global_path = temp.path().join("flowdex.toml");
        let cases = [
            (
                "compaction_reminder_threshold_tokens =",
                "invalid Flowdex config",
            ),
            ("other = 1", "unknown field `other`"),
            (
                "compaction_reminder_threshold_tokens = 0",
                "value must be positive",
            ),
            (
                "compaction_reminder_threshold_tokens = -1",
                "value must be positive",
            ),
            (
                "[tool_profiles.research]\nmodel = \"gpt-5\"",
                "unknown field `model`",
            ),
        ];
        for (source, expected) in cases {
            fs::write(&global_path, source).expect("global config");
            let error = load_config(temp.path(), None).expect_err(source);
            let message = error.to_string();
            assert!(message.contains(expected), "{message}");
            assert!(
                message.contains(global_path.to_string_lossy().as_ref()),
                "{message}"
            );
        }
    }

    #[test]
    fn rejects_empty_and_duplicate_ast_grep_rule_ids() {
        let temp = tempdir().expect("temp dir");
        let path = temp.path().join("flowdex.toml");
        for source in [
            "ast_grep_always_run = [\"\"]",
            "ast_grep_always_run = [\"same\", \"same\"]",
        ] {
            fs::write(&path, source).expect("global config");
            let error = load_config(temp.path(), None).expect_err(source);
            assert!(error.to_string().contains(path.to_string_lossy().as_ref()));
            assert!(error.to_string().contains("ast_grep_always_run"));
        }
    }

    #[test]
    fn resolves_ast_grep_candidate_threshold_with_global_and_repository_precedence() {
        let temp = tempdir().expect("temp dir");
        let repository_root = temp.path().join("repo");
        fs::create_dir_all(repository_root.join(".flowdex")).expect("repository config dir");

        assert_eq!(
            config(temp.path(), Some(&repository_root)).ast_grep_candidate_threshold,
            DEFAULT_AST_GREP_CANDIDATE_THRESHOLD
        );

        fs::write(
            temp.path().join("flowdex.toml"),
            "ast_grep_candidate_threshold = 7\n",
        )
        .expect("global config");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).ast_grep_candidate_threshold,
            7
        );

        fs::write(
            repository_root.join(".flowdex/config.toml"),
            "ast_grep_candidate_threshold = 11\n",
        )
        .expect("repository config");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).ast_grep_candidate_threshold,
            11
        );

        fs::write(repository_root.join(".flowdex/config.toml"), "# omitted\n")
            .expect("repository omission");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).ast_grep_candidate_threshold,
            7
        );
    }

    #[test]
    fn rejects_non_positive_ast_grep_candidate_threshold() {
        let temp = tempdir().expect("temp dir");
        let path = temp.path().join("flowdex.toml");
        for source in [
            "ast_grep_candidate_threshold = 0",
            "ast_grep_candidate_threshold = -1",
        ] {
            fs::write(&path, source).expect("global config");
            let error = load_config(temp.path(), None).expect_err(source);
            let message = error.to_string();
            assert!(
                message.contains("ast_grep_candidate_threshold"),
                "{message}"
            );
            assert!(message.contains("value must be positive"), "{message}");
            assert!(
                message.contains(path.to_string_lossy().as_ref()),
                "{message}"
            );
        }
    }
}
