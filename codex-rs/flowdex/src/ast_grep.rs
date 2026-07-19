use ast_grep_config::{CombinedScan, GlobalRules, RuleConfig, Severity, from_yaml_string};
use ast_grep_core::Language;
use ast_grep_core::tree_sitter::LanguageExt;
use ast_grep_language::SupportLang;
use ignore::WalkBuilder;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_FINDINGS: usize = 256;
const MAX_MESSAGE_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AstGrepFinding {
    pub rule_id: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
    pub severity: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AstGrepResult {
    pub passed: bool,
    pub findings: Vec<AstGrepFinding>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Error)]
pub enum AstGrepError {
    #[error("unable to resolve AST-grep repository root: {0}")]
    RepositoryRoot(#[source] std::io::Error),
    #[error("unable to resolve AST-grep execution root: {0}")]
    ExecutionRoot(#[source] std::io::Error),
    #[error("AST-grep rules directory is unavailable: {path}")]
    RulesDirectory { path: PathBuf },
    #[error("AST-grep rule path is outside the trusted rules directory: {path}")]
    RuleOutsideDirectory { path: PathBuf },
    #[error("unable to read AST-grep rule at {path}: {source}")]
    RuleRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid AST-grep rule at {path}: {message}")]
    RuleParse { path: PathBuf, message: String },
    #[error("AST-grep rule at {path} must not define fixes or rewrites")]
    RuleRewrite { path: PathBuf },
    #[error("AST-grep rule at {path} has an empty id")]
    EmptyRuleId { path: PathBuf },
    #[error("duplicate AST-grep rule id `{id}` at {path}")]
    DuplicateRuleId { id: String, path: PathBuf },
    #[error("AST-grep rule id `{id}` was not found")]
    MissingRule { id: String },
    #[error("duplicate AST-grep rule id `{id}` requested")]
    DuplicateRequestedRule { id: String },
    #[error("unable to read AST-grep source file at {path}: {source}")]
    SourceRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("unable to walk AST-grep execution root: {message}")]
    Walk { message: String },
    #[error("AST-grep scan cancelled")]
    Cancelled,
}

struct LoadedRule {
    rule: RuleConfig<SupportLang>,
}

/// Returns the exact native IDs declared by approved repository YAML rules.
pub fn discover_approved_rule_ids(
    trusted_repository_root: &Path,
) -> Result<BTreeSet<String>, AstGrepError> {
    let repository_root = trusted_repository_root
        .canonicalize()
        .map_err(AstGrepError::RepositoryRoot)?;
    let rules_root = repository_root
        .join(".flowdex")
        .join("ast-grep")
        .join("rules");
    let rules_root = rules_root
        .canonicalize()
        .map_err(|_| AstGrepError::RulesDirectory {
            path: rules_root.clone(),
        })?;
    if !rules_root.starts_with(&repository_root) {
        return Err(AstGrepError::RuleOutsideDirectory { path: rules_root });
    }
    let mut ids = BTreeSet::new();
    let entries = fs::read_dir(&rules_root).map_err(|_| AstGrepError::RulesDirectory {
        path: rules_root.clone(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| AstGrepError::RuleRead {
            path: rules_root.clone(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("yml") {
            continue;
        }
        let canonical = path
            .canonicalize()
            .map_err(|source| AstGrepError::RuleRead {
                path: path.clone(),
                source,
            })?;
        if !canonical.starts_with(&rules_root) {
            return Err(AstGrepError::RuleOutsideDirectory { path });
        }
        let source = fs::read_to_string(&canonical).map_err(|source| AstGrepError::RuleRead {
            path: canonical.clone(),
            source,
        })?;
        let registration = GlobalRules::default();
        let parsed = from_yaml_string::<SupportLang>(&source, &registration).map_err(|error| {
            AstGrepError::RuleParse {
                path: canonical.clone(),
                message: bounded_error_text(&error.to_string()),
            }
        })?;
        for rule in parsed {
            if rule.id.trim().is_empty() {
                return Err(AstGrepError::EmptyRuleId {
                    path: canonical.clone(),
                });
            }
            if rule.fix.is_some() || rule.rewriters.is_some() {
                return Err(AstGrepError::RuleRewrite {
                    path: canonical.clone(),
                });
            }
            if !ids.insert(rule.id.clone()) {
                return Err(AstGrepError::DuplicateRuleId {
                    id: rule.id.clone(),
                    path: canonical.clone(),
                });
            }
        }
    }
    Ok(ids)
}

/// Executes the exact native AST-grep rules selected by Flowdex configuration.
pub fn run_ast_grep_rules(
    trusted_repository_root: &Path,
    execution_root: &Path,
    exact_rule_ids: &[String],
) -> Result<AstGrepResult, AstGrepError> {
    run_ast_grep_rules_with_cancellation(
        trusted_repository_root,
        execution_root,
        exact_rule_ids,
        || false,
    )
}

/// Executes rules while allowing the caller to stop traversal and matching.
pub fn run_ast_grep_rules_with_cancellation<F>(
    trusted_repository_root: &Path,
    execution_root: &Path,
    exact_rule_ids: &[String],
    is_cancelled: F,
) -> Result<AstGrepResult, AstGrepError>
where
    F: Fn() -> bool,
{
    let repository_root = trusted_repository_root
        .canonicalize()
        .map_err(AstGrepError::RepositoryRoot)?;
    let execution_root = execution_root
        .canonicalize()
        .map_err(AstGrepError::ExecutionRoot)?;
    if is_cancelled() {
        return Err(AstGrepError::Cancelled);
    }

    let requested = requested_ids(exact_rule_ids)?;
    let rules_root = repository_root
        .join(".flowdex")
        .join("ast-grep")
        .join("rules");
    let rules_root = rules_root
        .canonicalize()
        .map_err(|_| AstGrepError::RulesDirectory {
            path: rules_root.clone(),
        })?;
    if !rules_root.starts_with(&repository_root) {
        return Err(AstGrepError::RuleOutsideDirectory { path: rules_root });
    }

    let mut all_rules = Vec::new();
    let entries = fs::read_dir(&rules_root).map_err(|_| AstGrepError::RulesDirectory {
        path: rules_root.clone(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| AstGrepError::RuleRead {
            path: rules_root.clone(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("yml") {
            continue;
        }
        let canonical = path
            .canonicalize()
            .map_err(|source| AstGrepError::RuleRead {
                path: path.clone(),
                source,
            })?;
        if !canonical.starts_with(&rules_root) {
            return Err(AstGrepError::RuleOutsideDirectory { path });
        }
        let source = fs::read_to_string(&canonical).map_err(|source| AstGrepError::RuleRead {
            path: canonical.clone(),
            source,
        })?;
        let registration = GlobalRules::default();
        let parsed = from_yaml_string::<SupportLang>(&source, &registration).map_err(|error| {
            AstGrepError::RuleParse {
                path: canonical.clone(),
                message: bounded_error_text(&error.to_string()),
            }
        })?;
        for rule in parsed {
            if rule.id.trim().is_empty() {
                return Err(AstGrepError::EmptyRuleId {
                    path: canonical.clone(),
                });
            }
            if rule.fix.is_some() || rule.rewriters.is_some() {
                return Err(AstGrepError::RuleRewrite {
                    path: canonical.clone(),
                });
            }
            if all_rules
                .iter()
                .any(|existing: &LoadedRule| existing.rule.id == rule.id)
            {
                return Err(AstGrepError::DuplicateRuleId {
                    id: rule.id.clone(),
                    path: canonical.clone(),
                });
            }
            all_rules.push(LoadedRule { rule });
        }
    }

    let selected: Vec<_> = all_rules
        .into_iter()
        .filter(|loaded| requested.contains(&loaded.rule.id))
        .collect();
    for id in &requested {
        if !selected.iter().any(|loaded| loaded.rule.id == *id) {
            return Err(AstGrepError::MissingRule { id: id.clone() });
        }
    }

    let mut by_language: HashMap<SupportLang, Vec<LoadedRule>> = HashMap::new();
    for loaded in selected {
        by_language
            .entry(loaded.rule.language)
            .or_default()
            .push(loaded);
    }

    let mut findings = Vec::new();
    let mut truncated = false;
    let walker = WalkBuilder::new(&execution_root).build();
    for entry in walker {
        if is_cancelled() {
            return Err(AstGrepError::Cancelled);
        }
        let entry = match entry {
            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => entry,
            Ok(_) => continue,
            Err(error) => {
                return Err(AstGrepError::Walk {
                    message: bounded_error_text(&error.to_string()),
                });
            }
        };
        let path = entry.path();
        let Some(language) = SupportLang::from_path(path) else {
            continue;
        };
        let Some(rules) = by_language.get(&language) else {
            continue;
        };
        let source = fs::read_to_string(path).map_err(|source| AstGrepError::SourceRead {
            path: path.to_path_buf(),
            source,
        })?;
        let ast = language.ast_grep(&source);
        let references: Vec<_> = rules
            .iter()
            .filter(|loaded| !matches!(loaded.rule.severity, Severity::Off))
            .map(|loaded| &loaded.rule)
            .collect();
        if references.is_empty() {
            continue;
        }
        let combined = CombinedScan::new(references);
        let scan = combined.scan(&ast, false);
        for (rule, matches) in scan.matches {
            for matched in matches {
                if findings.len() >= MAX_FINDINGS {
                    truncated = true;
                    break;
                }
                if is_cancelled() {
                    return Err(AstGrepError::Cancelled);
                }
                let relative = path
                    .strip_prefix(&execution_root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let position = matched.start_pos();
                let message = bounded_message(rule.get_message(&matched), &mut truncated);
                findings.push(AstGrepFinding {
                    rule_id: rule.id.clone(),
                    file: relative,
                    line: position.line() + 1,
                    column: position.column(&matched) + 1,
                    message,
                    severity: severity_name(&rule.severity).to_string(),
                });
            }
        }
    }

    findings.sort_by(|left, right| {
        (&left.rule_id, &left.file, left.line, left.column).cmp(&(
            &right.rule_id,
            &right.file,
            right.line,
            right.column,
        ))
    });
    Ok(AstGrepResult {
        passed: findings.is_empty(),
        findings,
        truncated,
    })
}

fn requested_ids(ids: &[String]) -> Result<BTreeSet<String>, AstGrepError> {
    let mut requested = BTreeSet::new();
    for id in ids {
        if id.trim().is_empty() {
            return Err(AstGrepError::MissingRule { id: id.clone() });
        }
        if !requested.insert(id.clone()) {
            return Err(AstGrepError::DuplicateRequestedRule { id: id.clone() });
        }
    }
    Ok(requested)
}

fn bounded_message(mut message: String, truncated: &mut bool) -> String {
    if message.len() <= MAX_MESSAGE_BYTES {
        return message;
    }
    *truncated = true;
    let end = utf8_boundary(&message, MAX_MESSAGE_BYTES);
    message.truncate(end);
    message
}

fn bounded_error_text(message: &str) -> String {
    const SUFFIX: &str = " … (truncated)";
    if message.len() <= MAX_MESSAGE_BYTES {
        return message.to_string();
    }
    let limit = MAX_MESSAGE_BYTES.saturating_sub(SUFFIX.len());
    let end = utf8_boundary(message, limit);
    format!("{}{}", &message[..end], SUFFIX)
}

fn utf8_boundary(text: &str, limit: usize) -> usize {
    let mut end = limit.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn severity_name(severity: &Severity) -> &'static str {
    match severity {
        Severity::Hint => "hint",
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
        Severity::Off => "off",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn loads_selects_and_reports_native_rule() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path().join("trusted");
        let execution = temp.path().join("linked-worktree");
        fs::create_dir_all(root.join(".flowdex/ast-grep/rules")).expect("rules dir");
        fs::create_dir_all(execution.join("src")).expect("source dir");
        fs::write(
            root.join(".flowdex/ast-grep/rules/no-console.yml"),
            "id: no-console\nlanguage: JavaScript\nrule:\n  pattern: console.log($$$ARGS)\nmessage: Avoid console output\nseverity: warning\n",
        )
        .expect("rule");
        fs::write(
            execution.join("src/main.js"),
            "const x = 1;\nconsole.log(x);\n",
        )
        .expect("source");

        let result =
            run_ast_grep_rules(&root, &execution, &["no-console".to_string()]).expect("run");
        assert!(!result.passed);
        assert!(!result.truncated);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].rule_id, "no-console");
        assert_eq!(result.findings[0].file, "src/main.js");
        assert_eq!(result.findings[0].line, 2);
        assert_eq!(result.findings[0].column, 1);
        assert_eq!(result.findings[0].severity, "warning");

        let passing = run_ast_grep_rules(&root, &execution, &["no-console".to_string()]);
        assert!(passing.is_ok());
    }

    #[test]
    fn cancellation_and_utf8_bounds_are_explicit() {
        let temp = tempdir().expect("temp dir");
        fs::create_dir_all(temp.path().join(".flowdex/ast-grep/rules")).expect("rules dir");
        let ids = ["rule".to_string()];
        assert!(matches!(
            run_ast_grep_rules_with_cancellation(temp.path(), temp.path(), &ids, || true),
            Err(AstGrepError::Cancelled)
        ));

        let bounded = bounded_error_text(&"é".repeat(MAX_MESSAGE_BYTES));
        assert!(bounded.len() <= MAX_MESSAGE_BYTES);
        assert!(bounded.ends_with(" … (truncated)"));
    }

    #[test]
    fn rejects_missing_and_duplicate_rule_selection() {
        let temp = tempdir().expect("temp dir");
        fs::create_dir_all(temp.path().join(".flowdex/ast-grep/rules")).expect("rules dir");
        let duplicate = vec!["missing".to_string(), "missing".to_string()];
        assert!(matches!(
            run_ast_grep_rules(temp.path(), temp.path(), &duplicate),
            Err(AstGrepError::DuplicateRequestedRule { .. })
        ));
    }

    #[test]
    fn discovers_exact_native_ids_from_all_approved_yaml() {
        let temp = tempdir().expect("temp dir");
        let rules = temp.path().join(".flowdex/ast-grep/rules");
        fs::create_dir_all(&rules).expect("rules dir");
        fs::write(
            rules.join("selected.yml"),
            "id: selected\nlanguage: JavaScript\nrule:\n  pattern: console.log($$$ARGS)\nmessage: selected\n",
        )
        .expect("rule");
        fs::write(
            rules.join("always.yml"),
            "id: always\nlanguage: JavaScript\nrule:\n  pattern: console.error($$$ARGS)\nmessage: always\n",
        )
        .expect("rule");
        let ids = discover_approved_rule_ids(temp.path()).expect("discover IDs");
        assert_eq!(ids.into_iter().collect::<Vec<_>>(), ["always", "selected"]);
    }
}
