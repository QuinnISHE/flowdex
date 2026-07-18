use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use thiserror::Error;

pub const DEFAULT_COMPACTION_REMINDER_THRESHOLD_TOKENS: i64 = 150_000;

/// Resolved Flowdex settings used by a session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlowdexConfig {
    pub compaction_reminder_threshold_tokens: i64,
    pub ast_grep_always_run: Vec<String>,
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
    let mut ast_grep_always_run = Vec::new();

    if let Some(config) = read_partial(&global_path)? {
        if let Some(value) = config.compaction_reminder_threshold_tokens {
            validate_threshold(value, &global_path)?;
            threshold = value;
        }
        if let Some(value) = config.ast_grep_always_run {
            validate_rule_ids(&value, &global_path)?;
            ast_grep_always_run = value;
        }
    }

    if let Some(repository_root) = trusted_repository_root {
        let repository_path = repository_root.join(".flowdex").join("config.toml");
        if let Some(config) = read_partial(&repository_path)? {
            if let Some(value) = config.compaction_reminder_threshold_tokens {
                validate_threshold(value, &repository_path)?;
                threshold = value;
            }
            if let Some(value) = config.ast_grep_always_run {
                validate_rule_ids(&value, &repository_path)?;
                ast_grep_always_run = value;
            }
        }
    }

    Ok(FlowdexConfig {
        compaction_reminder_threshold_tokens: threshold,
        ast_grep_always_run,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialFlowdexConfig {
    compaction_reminder_threshold_tokens: Option<i64>,
    ast_grep_always_run: Option<Vec<String>>,
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

    #[error(
        "invalid ast_grep_always_run rule id `{id}` in Flowdex config at {path}: id must be non-empty"
    )]
    InvalidAstGrepRuleId { path: PathBuf, id: String },

    #[error("duplicate ast_grep_always_run rule id `{id}` in Flowdex config at {path}")]
    DuplicateAstGrepRuleId { path: PathBuf, id: String },
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

        fs::write(
            temp.path().join("flowdex.toml"),
            "compaction_reminder_threshold_tokens = 120000\n",
        )
        .expect("global config");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).compaction_reminder_threshold_tokens,
            120000
        );

        fs::write(
            repository_root.join(".flowdex/config.toml"),
            "compaction_reminder_threshold_tokens = 90000\n",
        )
        .expect("repository config");
        assert_eq!(
            config(temp.path(), Some(&repository_root)).compaction_reminder_threshold_tokens,
            90000
        );

        fs::write(
            repository_root.join(".flowdex/config.toml"),
            "ast_grep_always_run = [\"repo-rule\"]\n",
        )
        .expect("repository omission");
        let resolved = config(temp.path(), Some(&repository_root));
        assert_eq!(resolved.compaction_reminder_threshold_tokens, 120000);
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
}
