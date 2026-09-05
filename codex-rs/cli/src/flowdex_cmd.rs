use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use codex_core::config::find_codex_home;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

const INSTALL_MANIFEST: &str = "flowdex/installed-assets-v1";
const FLOWDEX_CONFIG_PATH: &str = "flowdex.toml";
const FLOWDEX_CONFIG_CONTENTS: &str = "# Flowdex global defaults. Repository-local .flowdex settings take precedence.\n\
compaction_reminder_threshold_tokens = 185000\n\
verification_timeout_ms = 300000\n\
multi_agent_version = \"v1\"\n\
# Options: codex, claude, pi.\n\
system_prompt_mode = \"codex\"\n\
ast_grep_candidate_threshold = 3\n\
ast_grep_always_run = []\n\
subagent_excluded_tools = []\n\
subagent_excluded_skills = [\"run-flowdex-workflows\"]\n\
\n\
[tool_profiles]\n\
# Add named tool profiles as [tool_profiles.<name>] tables. Profiles may add\n\
# excluded_tools and excluded_skills for focused Flowdex child roles.\n";

#[cfg(windows)]
const PACKAGE_HELPERS: &[&str] = &[
    "codex-code-mode-host.exe",
    "codex-windows-sandbox-setup.exe",
    "codex-command-runner.exe",
];

#[cfg(target_os = "macos")]
const PACKAGE_HELPERS: &[&str] = &["codex-code-mode-host"];

const RETIRED_BUNDLED_ASSETS: &[&str] = &[
    "skills/collect-flowdex-context/SKILL.md",
    "skills/collect-flowdex-context/agents/openai.yaml",
    "skills/report-flowdex-review/SKILL.md",
    "skills/report-flowdex-review/agents/openai.yaml",
];

const UPDATED_BUNDLED_ASSETS: &[&str] = &[
    "skills/run-flowdex-workflows/SKILL.md",
    "skills/run-flowdex-workflows/agents/openai.yaml",
    "skills/run-flowdex-workflows/examples/implementation-run.js",
    "skills/run-flowdex-workflows/examples/agent-rounds.js",
    "skills/run-flowdex-workflows/examples/reusable-workflow.js",
];

struct BundledAsset {
    relative_path: &'static str,
    contents: &'static str,
}

const BUNDLED_ASSETS: &[BundledAsset] = &[
    BundledAsset {
        relative_path: FLOWDEX_CONFIG_PATH,
        contents: FLOWDEX_CONFIG_CONTENTS,
    },
    BundledAsset {
        relative_path: "flowdex/workflows/defaults/research-rounds.js",
        contents: include_str!("../../../.flowdex/workflows/defaults/research-rounds.js"),
    },
    BundledAsset {
        relative_path: "flowdex/workflows/defaults/worker-reviewer.js",
        contents: include_str!("../../../.flowdex/workflows/defaults/worker-reviewer.js"),
    },
    BundledAsset {
        relative_path: "skills/run-flowdex-workflows/SKILL.md",
        contents: include_str!("../../../.codex/skills/run-flowdex-workflows/SKILL.md"),
    },
    BundledAsset {
        relative_path: "skills/run-flowdex-workflows/agents/openai.yaml",
        contents: include_str!("../../../.codex/skills/run-flowdex-workflows/agents/openai.yaml"),
    },
    BundledAsset {
        relative_path: "skills/run-flowdex-workflows/examples/implementation-run.js",
        contents: include_str!(
            "../../../.codex/skills/run-flowdex-workflows/examples/implementation-run.js"
        ),
    },
    BundledAsset {
        relative_path: "skills/run-flowdex-workflows/examples/agent-rounds.js",
        contents: include_str!(
            "../../../.codex/skills/run-flowdex-workflows/examples/agent-rounds.js"
        ),
    },
    BundledAsset {
        relative_path: "skills/run-flowdex-workflows/examples/reusable-workflow.js",
        contents: include_str!(
            "../../../.codex/skills/run-flowdex-workflows/examples/reusable-workflow.js"
        ),
    },
];

#[derive(Debug, clap::Args)]
pub struct FlowdexCli {
    #[command(subcommand)]
    pub subcommand: FlowdexSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum FlowdexSubcommand {
    /// Install this Flowdex binary as the Codex desktop app backend.
    Install(InstallArgs),
    /// Remove the Flowdex desktop backend override and installed binary.
    Uninstall(UninstallArgs),
}

#[derive(Debug, clap::Args)]
pub struct InstallArgs {}

#[derive(Debug, clap::Args)]
pub struct UninstallArgs {
    /// Also remove global configuration, workflows, and skills created by the installer.
    #[arg(long)]
    pub purge: bool,
}

#[derive(Debug, Parser)]
#[command(name = "flowdex", bin_name = "flowdex")]
struct StandaloneFlowdexCli {
    #[command(subcommand)]
    subcommand: FlowdexSubcommand,
}

pub async fn run(cli: FlowdexCli) -> Result<()> {
    match cli.subcommand {
        FlowdexSubcommand::Install(_) => run_install().await,
        FlowdexSubcommand::Uninstall(args) => run_uninstall(args.purge).await,
    }
}

pub fn invoked_as_flowdex() -> bool {
    std::env::current_exe()
        .ok()
        .as_deref()
        .is_some_and(is_flowdex_executable)
}

fn is_flowdex_executable(path: &Path) -> bool {
    path.file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("flowdex"))
}

pub async fn run_standalone() -> Result<()> {
    let args = std::env::args_os().collect::<Vec<_>>();
    if is_standalone_version_request(&args[1..]) {
        println!("codex-cli {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    run(FlowdexCli {
        subcommand: StandaloneFlowdexCli::parse_from(args).subcommand,
    })
    .await
}

fn is_standalone_version_request(args: &[OsString]) -> bool {
    args.len() == 1 && args[0] == OsStr::new("--version")
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn installed_binary_path() -> Result<PathBuf> {
    let codex_home = find_codex_home().context("failed to resolve CODEX_HOME")?;
    let name = if cfg!(target_os = "windows") {
        "codex.exe"
    } else {
        "codex"
    };
    Ok(codex_home
        .join("flowdex")
        .join("bin")
        .join(name)
        .to_path_buf())
}

async fn run_install() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let helpers = package_helpers()?;
        let target = install_current_binary(BinaryValidation::Macos)?;
        install_package_helpers(&helpers, &target)?;
        install_bundled_assets(find_codex_home()?.as_path())?;
        macos::install(&target)?;
        println!(
            "Flowdex installed at {}. Fully quit and restart the Codex app.",
            target.display()
        );
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let helpers = package_helpers()?;
        let target = install_current_binary(BinaryValidation::Windows)?;
        install_package_helpers(&helpers, &target)?;
        install_bundled_assets(find_codex_home()?.as_path())?;
        let mut writer = RegistryEnvironmentWriter;
        writer.set_codex_cli_path(&target)?;
        println!(
            "Flowdex installed at {}. Fully quit and restart the Codex app.",
            target.display()
        );
        Ok(())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        anyhow::bail!("`flowdex install` is only supported on Windows and macOS");
    }
}

async fn run_uninstall(purge: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::uninstall()?;
        remove_installed_package_helpers()?;
        remove_flowdex_files(purge)?;
        print_uninstall_result(purge);
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let mut writer = RegistryEnvironmentWriter;
        writer.remove_codex_cli_path()?;
        remove_installed_package_helpers()?;
        remove_flowdex_files(purge)?;
        print_uninstall_result(purge);
        Ok(())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        anyhow::bail!("`flowdex uninstall` is only supported on Windows and macOS");
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn remove_flowdex_files(purge: bool) -> Result<()> {
    remove_installed_binary()?;
    if purge {
        purge_installed_assets(find_codex_home()?.as_path())
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn print_uninstall_result(purge: bool) {
    if purge {
        println!(
            "Flowdex uninstalled and installer-owned assets removed. Fully quit and restart the Codex app."
        );
    } else {
        println!("Flowdex uninstalled. Fully quit and restart the Codex app.");
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
enum BinaryValidation {
    Windows,
    Macos,
}

#[cfg(windows)]
trait EnvironmentWriter {
    fn set_codex_cli_path(&mut self, path: &Path) -> Result<()>;
    fn remove_codex_cli_path(&mut self) -> Result<()>;
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn install_current_binary(validation: BinaryValidation) -> Result<PathBuf> {
    let source = std::env::current_exe().context("cannot locate the running Flowdex binary")?;
    let target = installed_binary_path()?;
    install_binary(&source, &target, run_version_check, validation)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn package_helpers() -> Result<Vec<PathBuf>> {
    let package = std::env::current_exe().context("cannot locate the running Flowdex package")?;
    let directory = package
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Flowdex package path has no parent"))?;
    PACKAGE_HELPERS
        .iter()
        .map(|name| {
            let path = directory.join(name);
            let canonical = path
                .canonicalize()
                .with_context(|| format!("Flowdex package is missing {name}"))?;
            if !canonical.is_file() {
                anyhow::bail!(
                    "Flowdex package helper is not a regular file: {}",
                    canonical.display()
                );
            }
            Ok(canonical)
        })
        .collect()
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn install_package_helpers(helpers: &[PathBuf], binary: &Path) -> Result<()> {
    let directory = binary
        .parent()
        .ok_or_else(|| anyhow::anyhow!("installed Flowdex path has no parent"))?;
    for (name, source) in PACKAGE_HELPERS.iter().zip(helpers) {
        install_package_file(source, &directory.join(name))?;
    }
    Ok(())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn remove_installed_package_helpers() -> Result<()> {
    let binary = installed_binary_path()?;
    let directory = binary
        .parent()
        .ok_or_else(|| anyhow::anyhow!("installed Flowdex path has no parent"))?;
    for name in PACKAGE_HELPERS {
        remove_file_if_exists(&directory.join(name))?;
    }
    Ok(())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn install_package_file(source: &Path, target: &Path) -> Result<()> {
    if source == target {
        return Ok(());
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("installed package file has no parent"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".flowdex-install-{}-helper", std::process::id()));
    let _ = fs::remove_file(&temp);
    let result = (|| {
        fs::copy(source, &temp)
            .with_context(|| format!("cannot copy {} to {}", source.display(), temp.display()))?;
        if target.exists() {
            fs::remove_file(target)
                .with_context(|| format!("cannot replace {}", target.display()))?;
        }
        fs::rename(&temp, target)
            .with_context(|| format!("cannot install {}", target.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn install_binary<V>(
    source: &Path,
    target: &Path,
    version_check: V,
    validation: BinaryValidation,
) -> Result<PathBuf>
where
    V: FnOnce(&Path) -> Result<()>,
{
    let canonical = validate_binary(source, version_check, validation)?;
    if canonical == target {
        return Ok(target.to_path_buf());
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("installed binary path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".flowdex-install-{}", std::process::id()));
    let _ = fs::remove_file(&temp);
    let result = (|| {
        fs::copy(&canonical, &temp).with_context(|| {
            format!(
                "cannot copy Flowdex from {} to {}",
                canonical.display(),
                temp.display()
            )
        })?;
        if target.exists() {
            fs::remove_file(target).with_context(|| {
                format!("cannot replace installed Flowdex at {}", target.display())
            })?;
        }
        fs::rename(&temp, target)
            .with_context(|| format!("cannot install Flowdex at {}", target.display()))?;
        Ok(target.to_path_buf())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn validate_binary<V>(
    binary: &Path,
    version_check: V,
    validation: BinaryValidation,
) -> Result<PathBuf>
where
    V: FnOnce(&Path) -> Result<()>,
{
    let canonical = binary
        .canonicalize()
        .with_context(|| format!("cannot locate Flowdex package: {}", binary.display()))?;
    let metadata = std::fs::metadata(&canonical)
        .with_context(|| format!("cannot inspect Flowdex package: {}", canonical.display()))?;
    if !metadata.is_file() {
        anyhow::bail!(
            "Flowdex package must be a regular file: {}",
            canonical.display()
        );
    }
    if matches!(validation, BinaryValidation::Windows) {
        let is_exe = canonical
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"));
        if !is_exe {
            anyhow::bail!(
                "Flowdex package must be a .exe file: {}",
                canonical.display()
            );
        }
    }
    if matches!(validation, BinaryValidation::Macos) {
        #[cfg(target_family = "unix")]
        {
            use std::os::unix::fs::PermissionsExt;
            if canonical.metadata()?.permissions().mode() & 0o111 == 0 {
                anyhow::bail!(
                    "Flowdex package must be executable: {}",
                    canonical.display()
                );
            }
        }
    }

    version_check(&canonical)?;
    Ok(canonical)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn remove_installed_binary() -> Result<()> {
    let path = installed_binary_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot remove {}", path.display())),
    }
}

fn install_bundled_assets(codex_home: &Path) -> Result<()> {
    let manifest_path = codex_home.join(INSTALL_MANIFEST);
    let known_paths = BUNDLED_ASSETS
        .iter()
        .map(|asset| asset.relative_path)
        .chain(RETIRED_BUNDLED_ASSETS.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut installed_paths = match fs::read_to_string(&manifest_path) {
        Ok(manifest) => manifest
            .lines()
            .filter(|path| known_paths.contains(*path))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", manifest_path.display()));
        }
    };

    for relative_path in RETIRED_BUNDLED_ASSETS {
        if installed_paths.remove(*relative_path) {
            remove_file_if_exists(&codex_home.join(relative_path))?;
        }
    }
    for relative_path in [
        "skills/collect-flowdex-context/agents",
        "skills/collect-flowdex-context",
        "skills/report-flowdex-review/agents",
        "skills/report-flowdex-review",
    ] {
        remove_empty_dir(&codex_home.join(relative_path))?;
    }

    for asset in BUNDLED_ASSETS {
        let path = codex_home.join(asset.relative_path);
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("bundled asset path has no parent"))?;
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
        if installed_paths.contains(asset.relative_path)
            && UPDATED_BUNDLED_ASSETS.contains(&asset.relative_path)
        {
            fs::write(&path, asset.contents)
                .with_context(|| format!("cannot update {}", path.display()))?;
            continue;
        }
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot create {}", path.display()));
            }
        };
        if let Err(error) = file.write_all(asset.contents.as_bytes()) {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(error).with_context(|| format!("cannot write {}", path.display()));
        }
        installed_paths.insert(asset.relative_path.to_owned());
    }

    complete_existing_flowdex_config(&codex_home.join(FLOWDEX_CONFIG_PATH))?;

    if installed_paths.is_empty() {
        remove_file_if_exists(&manifest_path)?;
    } else {
        let contents = installed_paths.into_iter().collect::<Vec<_>>().join("\n") + "\n";
        fs::write(&manifest_path, contents)
            .with_context(|| format!("cannot write {}", manifest_path.display()))?;
    }
    Ok(())
}

fn complete_existing_flowdex_config(path: &Path) -> Result<()> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let table = match toml::from_str::<toml::Table>(&source) {
        Ok(table) => table,
        Err(_) => return Ok(()),
    };

    let defaults = [
        (
            "compaction_reminder_threshold_tokens",
            "compaction_reminder_threshold_tokens = 185000\n",
        ),
        (
            "verification_timeout_ms",
            "verification_timeout_ms = 300000\n",
        ),
        ("multi_agent_version", "multi_agent_version = \"v1\"\n"),
        ("system_prompt_mode", "system_prompt_mode = \"codex\"\n"),
        (
            "ast_grep_candidate_threshold",
            "ast_grep_candidate_threshold = 3\n",
        ),
        ("ast_grep_always_run", "ast_grep_always_run = []\n"),
        ("subagent_excluded_tools", "subagent_excluded_tools = []\n"),
        (
            "subagent_excluded_skills",
            "subagent_excluded_skills = [\"run-flowdex-workflows\"]\n",
        ),
    ];
    let mut prefix = String::new();
    for (key, default) in defaults {
        if !table.contains_key(key) {
            prefix.push_str(default);
        }
    }
    let add_tool_profiles = !table.contains_key("tool_profiles");
    if prefix.is_empty() && !add_tool_profiles {
        return Ok(());
    }

    let mut completed = prefix;
    completed.push_str(&source);
    if add_tool_profiles {
        if !completed.ends_with('\n') {
            completed.push('\n');
        }
        completed.push_str(
            "\n[tool_profiles]\n# Add named tool profiles as [tool_profiles.<name>] tables.\n",
        );
    }
    fs::write(path, completed).with_context(|| format!("cannot update {}", path.display()))
}

fn purge_installed_assets(codex_home: &Path) -> Result<()> {
    let manifest_path = codex_home.join(INSTALL_MANIFEST);
    let manifest = match fs::read_to_string(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", manifest_path.display()));
        }
    };
    let known_paths = BUNDLED_ASSETS
        .iter()
        .map(|asset| asset.relative_path)
        .chain(RETIRED_BUNDLED_ASSETS.iter().copied())
        .collect::<BTreeSet<_>>();
    for relative_path in manifest.lines().filter(|path| known_paths.contains(*path)) {
        remove_file_if_exists(&codex_home.join(relative_path))?;
    }
    remove_file_if_exists(&manifest_path)?;

    for relative_path in [
        "flowdex/bin",
        "flowdex/workflows/defaults",
        "flowdex/workflows",
        "flowdex",
        "skills/collect-flowdex-context/agents",
        "skills/collect-flowdex-context",
        "skills/report-flowdex-review/agents",
        "skills/report-flowdex-review",
        "skills/run-flowdex-workflows/examples",
        "skills/run-flowdex-workflows/agents",
        "skills/run-flowdex-workflows",
    ] {
        remove_empty_dir(&codex_home.join(relative_path))?;
    }
    Ok(())
}

fn remove_empty_dir(path: &Path) -> Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error).with_context(|| format!("cannot remove {}", path.display())),
    }
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot remove {}", path.display())),
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn run_version_check(path: &Path) -> Result<()> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .with_context(|| format!("failed to run {} --version", path.display()))?;
    validate_version_output(output.status.success(), &output.stdout, &output.stderr).with_context(
        || {
            format!(
                "{} --version did not identify a Codex binary",
                path.display()
            )
        },
    )
}

#[cfg(any(target_os = "macos", test))]
mod macos {
    #[cfg(target_os = "macos")]
    use anyhow::Context;
    use anyhow::Result;
    #[cfg(target_os = "macos")]
    use std::fs;
    #[cfg(target_os = "macos")]
    use std::io::ErrorKind;
    #[cfg(target_os = "macos")]
    use std::io::Write;
    use std::path::Path;
    use std::path::PathBuf;
    const LABEL: &str = "com.openai.flowdex-codex-cli-path";

    fn launch_agent_path(home: &Path) -> Result<PathBuf> {
        if !home.is_absolute() && !home.to_string_lossy().starts_with('/') {
            anyhow::bail!("HOME must be an absolute path");
        }
        Ok(home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LABEL}.plist")))
    }

    fn xml_escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    fn launch_agent_plist(canonical: &Path) -> Result<Vec<u8>> {
        let canonical = canonical
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("installed Flowdex path is not valid UTF-8"))?;
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
  <key>Label</key>\n  <string>{LABEL}</string>\n\
  <key>ProgramArguments</key>\n  <array>\n\
    <string>/bin/launchctl</string>\n    <string>setenv</string>\n\
    <string>CODEX_CLI_PATH</string>\n    <string>{}</string>\n\
  </array>\n  <key>RunAtLoad</key>\n  <true/>\n\
</dict>\n</plist>\n",
            xml_escape(canonical)
        )
        .into_bytes())
    }

    fn install_with<W, R>(
        home: &Path,
        uid: u32,
        canonical: &Path,
        writer: W,
        mut run: R,
    ) -> Result<()>
    where
        W: FnOnce(&Path, &[u8]) -> Result<()>,
        R: FnMut(&[String], bool) -> Result<()>,
    {
        let plist = launch_agent_path(home)?;
        writer(&plist, &launch_agent_plist(canonical)?)?;
        let domain = format!("gui/{uid}");
        let plist = plist.to_string_lossy().into_owned();
        run(&["bootout".into(), domain.clone(), plist.clone()], true)?;
        run(&["bootstrap".into(), domain, plist], false)?;
        run(
            &[
                "setenv".into(),
                "CODEX_CLI_PATH".into(),
                canonical.to_string_lossy().into_owned(),
            ],
            false,
        )
    }

    fn uninstall_with<R, D>(home: &Path, uid: u32, mut run: R, remove: D) -> Result<()>
    where
        R: FnMut(&[String], bool) -> Result<()>,
        D: FnOnce(&Path) -> Result<()>,
    {
        let plist = launch_agent_path(home)?;
        run(
            &[
                "bootout".into(),
                format!("gui/{uid}"),
                plist.to_string_lossy().into_owned(),
            ],
            true,
        )?;
        remove(&plist)?;
        run(&["unsetenv".into(), "CODEX_CLI_PATH".into()], false)
    }

    #[cfg(target_os = "macos")]
    fn write_launch_agent_atomic(path: &Path, contents: &[u8]) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("profile path has no parent directory"))?;
        fs::create_dir_all(parent)?;
        let mode = fs::metadata(path)
            .ok()
            .map(|metadata| {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode()
            })
            .unwrap_or(0o600);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let mut temp = None;
        let mut file = None;
        for attempt in 0..16 {
            let candidate = parent.join(format!(
                ".flowdex-launch-agent-{}-{nonce}-{attempt}",
                std::process::id()
            ));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(created) => {
                    temp = Some(candidate);
                    file = Some(created);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error).context("cannot create temporary LaunchAgent"),
            }
        }
        let temp = temp.ok_or_else(|| anyhow::anyhow!("cannot allocate temporary LaunchAgent"))?;
        let result = (|| {
            let mut file = file.take().expect("temporary profile file");
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
            file.write_all(contents)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, path)
                .with_context(|| format!("cannot replace LaunchAgent {}", path.display()))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    #[cfg(target_os = "macos")]
    fn uid() -> Result<u32> {
        let output = std::process::Command::new("id").arg("-u").output()?;
        if !output.status.success() {
            anyhow::bail!("`id -u` failed");
        }
        std::str::from_utf8(&output.stdout)?
            .trim()
            .parse()
            .context("`id -u` returned an invalid user ID")
    }

    #[cfg(target_os = "macos")]
    fn run_launchctl(args: &[String], ignore_failure: bool) -> Result<()> {
        let status = std::process::Command::new("launchctl")
            .args(args)
            .status()?;
        if status.success() || ignore_failure {
            Ok(())
        } else {
            anyhow::bail!("launchctl {} failed", args.join(" "))
        }
    }

    #[cfg(target_os = "macos")]
    pub fn install(canonical: &Path) -> Result<()> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot locate LaunchAgents"))?;
        install_with(
            &home,
            uid()?,
            canonical,
            write_launch_agent_atomic,
            run_launchctl,
        )
    }

    #[cfg(target_os = "macos")]
    pub fn uninstall() -> Result<()> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot locate LaunchAgents"))?;
        uninstall_with(&home, uid()?, run_launchctl, |path| {
            match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(error) => {
                    Err(error).with_context(|| format!("cannot remove {}", path.display()))
                }
            }
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn launch_agent_install_is_idempotent_and_updates_current_session() {
            let mut writes = Vec::new();
            let mut commands = Vec::new();
            for _ in 0..2 {
                install_with(
                    Path::new("/Users/test"),
                    501,
                    Path::new("/tmp/a&b/codex"),
                    |path, contents| {
                        writes.push((path.to_path_buf(), contents.to_vec()));
                        Ok(())
                    },
                    |args, ignored| {
                        commands.push((args.to_vec(), ignored));
                        Ok(())
                    },
                )
                .unwrap();
            }
            assert_eq!(writes[0], writes[1]);
            assert!(String::from_utf8_lossy(&writes[0].1).contains("/tmp/a&amp;b/codex"));
            assert_eq!(commands[0].0[0], "bootout");
            assert!(commands[0].1);
            assert_eq!(commands[1].0[0], "bootstrap");
            assert_eq!(
                commands[2].0,
                ["setenv", "CODEX_CLI_PATH", "/tmp/a&b/codex"]
            );
        }

        #[test]
        fn launch_agent_uninstall_unloads_removes_and_unsets() {
            let mut commands = Vec::new();
            let mut removed = None;
            uninstall_with(
                Path::new("/Users/test"),
                501,
                |args, ignored| {
                    commands.push((args.to_vec(), ignored));
                    Ok(())
                },
                |path| {
                    removed = Some(path.to_path_buf());
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(commands[0].0[0], "bootout");
            assert!(commands[0].1);
            assert_eq!(commands[1].0, ["unsetenv", "CODEX_CLI_PATH"]);
            assert_eq!(
                removed.unwrap(),
                launch_agent_path(Path::new("/Users/test")).unwrap()
            );
        }
    }
}

fn validate_version_output(success: bool, stdout: &[u8], _stderr: &[u8]) -> Result<()> {
    if !success {
        anyhow::bail!("version command failed");
    }
    let output = std::str::from_utf8(stdout).context("version output was not UTF-8")?;
    let output = output.strip_suffix('\n').unwrap_or(output);
    let output = output.strip_suffix('\r').unwrap_or(output);
    let Some(version) = output.strip_prefix("codex-cli ") else {
        anyhow::bail!("version output does not match `codex-cli <version>`");
    };
    if version.is_empty()
        || version.chars().any(char::is_whitespace)
        || !version
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".+-".contains(character))
    {
        anyhow::bail!("version output does not match `codex-cli <version>`");
    }
    Ok(())
}

#[cfg(windows)]
struct RegistryEnvironmentWriter;

#[cfg(windows)]
impl EnvironmentWriter for RegistryEnvironmentWriter {
    fn set_codex_cli_path(&mut self, path: &Path) -> Result<()> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;
        use windows_sys::Win32::System::Registry::KEY_SET_VALUE;
        use windows_sys::Win32::System::Registry::REG_OPTION_NON_VOLATILE;
        use windows_sys::Win32::System::Registry::REG_SZ;
        use windows_sys::Win32::System::Registry::RegCloseKey;
        use windows_sys::Win32::System::Registry::RegCreateKeyExW;
        use windows_sys::Win32::System::Registry::RegSetValueExW;

        let key_name: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
        let value_name: Vec<u16> = "CODEX_CLI_PATH".encode_utf16().chain([0]).collect();
        let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
        value.push(0);
        let mut key = 0;
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                key_name.as_ptr(),
                0,
                std::ptr::null_mut(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            anyhow::bail!("RegCreateKeyExW failed with Windows error {status}");
        }

        let status = unsafe {
            RegSetValueExW(
                key,
                value_name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * std::mem::size_of::<u16>()) as u32,
            )
        };
        unsafe {
            RegCloseKey(key);
        }
        if status != 0 {
            anyhow::bail!("RegSetValueExW failed with Windows error {status}");
        }
        Ok(())
    }

    fn remove_codex_cli_path(&mut self) -> Result<()> {
        use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
        use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;
        use windows_sys::Win32::System::Registry::KEY_SET_VALUE;
        use windows_sys::Win32::System::Registry::RegCloseKey;
        use windows_sys::Win32::System::Registry::RegDeleteValueW;
        use windows_sys::Win32::System::Registry::RegOpenKeyExW;

        let key_name: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
        let value_name: Vec<u16> = "CODEX_CLI_PATH".encode_utf16().chain([0]).collect();
        let mut key = 0;
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                key_name.as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut key,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        if status != 0 {
            anyhow::bail!("RegOpenKeyExW failed with Windows error {status}");
        }
        let status = unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
        unsafe {
            RegCloseKey(key);
        }
        if status != 0 && status != ERROR_FILE_NOT_FOUND {
            anyhow::bail!("RegDeleteValueW failed with Windows error {status}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use tempfile::tempdir;

    fn binary_path(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, b"binary").expect("binary");
        (dir, path)
    }

    #[test]
    fn rejects_non_exe_and_directories_before_version_check() {
        let (dir, non_exe) = binary_path("codex");
        let called = Cell::new(false);
        let error = validate_binary(
            &non_exe,
            |_| {
                called.set(true);
                Ok(())
            },
            BinaryValidation::Windows,
        )
        .expect_err("non-exe should fail");
        assert!(error.to_string().contains(".exe"));
        assert!(!called.get());

        let error = validate_binary(dir.path(), |_| Ok(()), BinaryValidation::Windows)
            .expect_err("directory should fail");
        assert!(error.to_string().contains("regular file"));
    }

    #[test]
    fn missing_binary_fails_without_installing() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("missing.exe");
        let target = dir.path().join("installed").join("codex.exe");
        let error = install_binary(&path, &target, |_| Ok(()), BinaryValidation::Windows)
            .expect_err("missing");
        assert!(error.to_string().contains("cannot locate Flowdex package"));
        assert!(!target.exists());
    }

    #[test]
    fn version_output_must_succeed_and_identify_codex() {
        assert!(validate_version_output(true, b"codex-cli 1.2.3", b"").is_ok());
        assert!(validate_version_output(true, b"codex-cli 1.2.3\n", b"").is_ok());
        assert!(validate_version_output(false, b"codex-cli 1.2.3", b"").is_err());
        assert!(validate_version_output(true, b"not a codex-compatible tool", b"").is_err());
        assert!(validate_version_output(true, b"", b"codex-cli 1.2.3").is_err());
        assert!(validate_version_output(true, b"codex 1.2.3", b"").is_err());
        assert!(validate_version_output(true, b"codex-cli", b"").is_err());
    }

    #[test]
    fn successful_install_copies_only_after_validation() {
        let (_dir, path) = binary_path("codex.exe");
        let target_dir = tempdir().expect("target dir");
        let target = target_dir.path().join("bin").join("codex.exe");
        let installed = install_binary(
            &path,
            &target,
            |validated| {
                assert!(validated.is_absolute());
                Ok(())
            },
            BinaryValidation::Windows,
        )
        .expect("install");
        assert_eq!(installed, target);
        assert_eq!(std::fs::read(installed).unwrap(), b"binary");
    }

    #[cfg(windows)]
    #[test]
    fn installs_every_required_windows_package_helper() {
        let package = tempdir().expect("package dir");
        let install = tempdir().expect("install dir");
        let binary = install.path().join("bin").join("codex.exe");
        let mut helpers = Vec::new();
        for (index, name) in PACKAGE_HELPERS.iter().enumerate() {
            let source = package.path().join(name);
            std::fs::write(&source, format!("helper-{index}")).expect("package helper");
            helpers.push(source);
        }

        install_package_helpers(&helpers, &binary).expect("install helpers");

        for (index, name) in PACKAGE_HELPERS.iter().enumerate() {
            assert_eq!(
                std::fs::read_to_string(install.path().join("bin").join(name))
                    .expect("installed helper"),
                format!("helper-{index}")
            );
        }
        assert!(PACKAGE_HELPERS.contains(&"codex-code-mode-host.exe"));
    }

    #[test]
    fn standalone_name_and_commands_are_exact() {
        assert!(is_flowdex_executable(Path::new("flowdex.exe")));
        assert!(is_flowdex_executable(Path::new("/tmp/Flowdex")));
        assert!(!is_flowdex_executable(Path::new("codex.exe")));
        assert!(StandaloneFlowdexCli::try_parse_from(["flowdex", "install"]).is_ok());
        assert!(is_standalone_version_request(&[OsString::from(
            "--version"
        )]));
        assert!(!is_standalone_version_request(&[
            OsString::from("--version"),
            OsString::from("install")
        ]));
        let uninstall =
            StandaloneFlowdexCli::try_parse_from(["flowdex", "uninstall"]).expect("uninstall");
        assert!(matches!(
            uninstall.subcommand,
            FlowdexSubcommand::Uninstall(UninstallArgs { purge: false })
        ));
        let purge = StandaloneFlowdexCli::try_parse_from(["flowdex", "uninstall", "--purge"])
            .expect("purge uninstall");
        assert!(matches!(
            purge.subcommand,
            FlowdexSubcommand::Uninstall(UninstallArgs { purge: true })
        ));
        assert!(
            StandaloneFlowdexCli::try_parse_from(["flowdex", "install", "--binary", "old.exe"])
                .is_err()
        );
    }

    #[test]
    fn install_bundled_assets_provisions_a_fresh_codex_home() {
        let codex_home = tempdir().expect("codex home");
        install_bundled_assets(codex_home.path()).expect("install assets");

        for asset in BUNDLED_ASSETS {
            assert_eq!(
                fs::read_to_string(codex_home.path().join(asset.relative_path))
                    .expect("installed asset"),
                asset.contents
            );
        }
        let manifest =
            fs::read_to_string(codex_home.path().join(INSTALL_MANIFEST)).expect("install manifest");
        assert_eq!(manifest.lines().count(), BUNDLED_ASSETS.len());
    }

    #[test]
    fn bundled_defaults_use_packaged_agent_selectors_and_exact_round_operations() {
        let config = BUNDLED_ASSETS
            .iter()
            .find(|asset| asset.relative_path == "flowdex.toml")
            .unwrap()
            .contents;
        let research = include_str!("../../../.flowdex/workflows/defaults/research-rounds.js");
        let worker = include_str!("../../../.flowdex/workflows/defaults/worker-reviewer.js");

        assert!(config.contains("multi_agent_version = \"v1\""));
        assert!(config.contains("system_prompt_mode = \"codex\""));
        assert!(config.contains("compaction_reminder_threshold_tokens = 185000"));
        assert!(config.contains("verification_timeout_ms = 300000"));
        assert!(config.contains("subagent_excluded_tools = []"));
        assert!(config.contains("subagent_excluded_skills = [\"run-flowdex-workflows\"]"));
        assert!(config.contains("[tool_profiles]"));
        for relative_path in [
            "skills/run-flowdex-workflows/SKILL.md",
            "skills/run-flowdex-workflows/agents/openai.yaml",
            "skills/run-flowdex-workflows/examples/implementation-run.js",
            "skills/run-flowdex-workflows/examples/agent-rounds.js",
            "skills/run-flowdex-workflows/examples/reusable-workflow.js",
        ] {
            assert!(
                BUNDLED_ASSETS
                    .iter()
                    .any(|asset| asset.relative_path == relative_path)
            );
        }
        assert!(!research.contains("profile:"));
        assert!(research.contains("flowdex.sendMessage("));
        assert!(research.contains("flowdex.resumeAgent("));
        assert!(research.contains("contextMode: \"keep\""));
        assert!(!research.contains("delivery: \"turn\""));
        assert!(!worker.contains("profile:"));
        assert!(worker.contains("model: \"gpt-5.6-sol\""));
        assert!(worker.contains("model: \"gpt-5.6-luna\""));
    }

    #[test]
    fn install_bundled_assets_preserves_existing_files() {
        let codex_home = tempdir().expect("codex home");
        let config = codex_home.path().join("flowdex.toml");
        let workflow = codex_home
            .path()
            .join("flowdex/workflows/defaults/research-rounds.js");
        fs::create_dir_all(workflow.parent().expect("workflow parent"))
            .expect("workflow directory");
        fs::write(&config, b"user config").expect("user config");
        fs::write(&workflow, b"user workflow").expect("user workflow");

        install_bundled_assets(codex_home.path()).expect("install assets");

        assert_eq!(fs::read(&config).expect("config remains"), b"user config");
        assert_eq!(
            fs::read(&workflow).expect("workflow remains"),
            b"user workflow"
        );
        let manifest =
            fs::read_to_string(codex_home.path().join(INSTALL_MANIFEST)).expect("install manifest");
        assert!(!manifest.lines().any(|path| path == "flowdex.toml"));
        assert!(
            !manifest
                .lines()
                .any(|path| path == "flowdex/workflows/defaults/research-rounds.js")
        );
    }

    #[test]
    fn install_completes_valid_existing_config_without_overwriting_values() {
        let codex_home = tempdir().expect("codex home");
        let config = codex_home.path().join("flowdex.toml");
        fs::write(
            &config,
            "# user setting\ncompaction_reminder_threshold_tokens = 210000\n",
        )
        .expect("user config");

        install_bundled_assets(codex_home.path()).expect("install assets");

        let source = fs::read_to_string(&config).expect("completed config");
        let table = toml::from_str::<toml::Table>(&source).expect("valid config");
        assert_eq!(
            table["compaction_reminder_threshold_tokens"].as_integer(),
            Some(210000)
        );
        assert_eq!(table["verification_timeout_ms"].as_integer(), Some(300000));
        assert_eq!(table["multi_agent_version"].as_str(), Some("v1"));
        assert_eq!(table["system_prompt_mode"].as_str(), Some("codex"));
        assert_eq!(table["ast_grep_candidate_threshold"].as_integer(), Some(3));
        assert_eq!(
            table["ast_grep_always_run"].as_array().map(Vec::len),
            Some(0)
        );
        assert_eq!(
            table["subagent_excluded_tools"].as_array().map(Vec::len),
            Some(0)
        );
        assert_eq!(
            table["subagent_excluded_skills"]
                .as_array()
                .and_then(|values| values.first())
                .and_then(toml::Value::as_str),
            Some("run-flowdex-workflows")
        );
        assert!(table["tool_profiles"].as_table().is_some());
        assert!(source.contains("# user setting"));
        let manifest =
            fs::read_to_string(codex_home.path().join(INSTALL_MANIFEST)).expect("manifest");
        assert!(!manifest.lines().any(|path| path == FLOWDEX_CONFIG_PATH));
    }

    #[test]
    fn reinstall_removes_only_installer_owned_retired_skills() {
        let codex_home = tempdir().expect("codex home");
        let retired = codex_home.path().join(RETIRED_BUNDLED_ASSETS[0]);
        let preserved = codex_home.path().join(RETIRED_BUNDLED_ASSETS[2]);
        fs::create_dir_all(retired.parent().expect("retired parent")).expect("retired directory");
        fs::create_dir_all(preserved.parent().expect("preserved parent"))
            .expect("preserved directory");
        fs::write(&retired, "installed").expect("retired skill");
        fs::write(&preserved, "user owned").expect("preserved skill");
        fs::create_dir_all(codex_home.path().join("flowdex")).expect("manifest directory");
        fs::write(
            codex_home.path().join(INSTALL_MANIFEST),
            format!("{}\n", RETIRED_BUNDLED_ASSETS[0]),
        )
        .expect("manifest");

        install_bundled_assets(codex_home.path()).expect("reinstall assets");

        assert!(!retired.exists());
        assert_eq!(
            fs::read_to_string(preserved).expect("user skill remains"),
            "user owned"
        );
        let manifest =
            fs::read_to_string(codex_home.path().join(INSTALL_MANIFEST)).expect("manifest");
        assert!(
            !manifest
                .lines()
                .any(|path| RETIRED_BUNDLED_ASSETS.contains(&path))
        );
    }

    #[test]
    fn reinstall_updates_only_installer_owned_workflow_skill() {
        let codex_home = tempdir().expect("codex home");
        install_bundled_assets(codex_home.path()).expect("initial install");
        let installed = codex_home.path().join(UPDATED_BUNDLED_ASSETS[0]);
        fs::write(&installed, "old installed skill").expect("stale installed skill");

        let user_home = tempdir().expect("user codex home");
        let user_skill = user_home.path().join(UPDATED_BUNDLED_ASSETS[0]);
        fs::create_dir_all(user_skill.parent().expect("skill parent")).expect("skill directory");
        fs::write(&user_skill, "user skill").expect("user skill");

        install_bundled_assets(codex_home.path()).expect("reinstall");
        install_bundled_assets(user_home.path()).expect("install around user skill");

        assert_eq!(
            fs::read_to_string(installed).expect("updated installed skill"),
            BUNDLED_ASSETS
                .iter()
                .find(|asset| asset.relative_path == UPDATED_BUNDLED_ASSETS[0])
                .expect("bundled skill")
                .contents
        );
        assert_eq!(
            fs::read_to_string(user_skill).expect("user skill remains"),
            "user skill"
        );
    }

    #[test]
    fn purge_removes_only_installer_owned_assets() {
        let codex_home = tempdir().expect("codex home");
        let existing_workflow = codex_home
            .path()
            .join("flowdex/workflows/defaults/worker-reviewer.js");
        fs::create_dir_all(existing_workflow.parent().expect("workflow parent"))
            .expect("workflow directory");
        fs::write(&existing_workflow, b"user workflow").expect("user workflow");
        install_bundled_assets(codex_home.path()).expect("install assets");
        let user_data = codex_home.path().join("flowdex/history.sqlite");
        fs::write(&user_data, b"history").expect("runtime data");
        fs::write(codex_home.path().join("config.toml"), b"codex").expect("codex config");

        purge_installed_assets(codex_home.path()).expect("purge");

        assert!(!codex_home.path().join("flowdex.toml").exists());
        assert_eq!(
            fs::read(&existing_workflow).expect("workflow remains"),
            b"user workflow"
        );
        assert_eq!(fs::read(&user_data).expect("history remains"), b"history");
        assert!(!codex_home.path().join(INSTALL_MANIFEST).exists());
        assert_eq!(
            fs::read(codex_home.path().join("config.toml")).expect("codex config remains"),
            b"codex"
        );
    }
}
