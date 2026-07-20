use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use codex_core::config::find_codex_home;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

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
    /// Also remove global Flowdex workflows, configuration, and runtime data.
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
        let target = install_current_binary(BinaryValidation::Macos)?;
        macos::install(&target)?;
        println!(
            "Flowdex installed at {}. Fully quit and restart the Codex app.",
            target.display()
        );
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let target = install_current_binary(BinaryValidation::Windows)?;
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
        remove_flowdex_files(purge)?;
        print_uninstall_result(purge);
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let mut writer = RegistryEnvironmentWriter;
        writer.remove_codex_cli_path()?;
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
    if purge {
        purge_flowdex_data(find_codex_home()?.as_path())
    } else {
        remove_installed_binary()
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn print_uninstall_result(purge: bool) {
    if purge {
        println!(
            "Flowdex uninstalled and global Flowdex data removed. Fully quit and restart the Codex app."
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

fn purge_flowdex_data(codex_home: &Path) -> Result<()> {
    remove_path_if_exists(&codex_home.join("flowdex"))?;
    remove_file_if_exists(&codex_home.join("flowdex.toml"))
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", path.display()));
        }
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .with_context(|| format!("cannot remove {}", path.display()))
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

    const START_MARKER: &[u8] = b"# >>> flowdex managed CODEX_CLI_PATH >>>";
    const END_MARKER: &[u8] = b"# <<< flowdex managed CODEX_CLI_PATH <<<";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Shell {
        Zsh,
        Bash,
        Fish,
    }

    pub fn shell_and_profile(shell: &str, home: &Path) -> Result<(Shell, PathBuf)> {
        if !home.is_absolute() && !home.to_string_lossy().starts_with('/') {
            anyhow::bail!("HOME must be an absolute path");
        }
        let name = Path::new(shell)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow::anyhow!("SHELL is not set to a supported login shell"))?;
        match name {
            "zsh" => Ok((Shell::Zsh, home.join(".zprofile"))),
            "bash" => Ok((Shell::Bash, home.join(".bash_profile"))),
            "fish" => Ok((
                Shell::Fish,
                home.join(".config")
                    .join("fish")
                    .join("conf.d")
                    .join("flowdex.fish"),
            )),
            _ => anyhow::bail!("unsupported login shell `{shell}`"),
        }
    }

    fn quote_path(path: &Path) -> String {
        let path = path.to_string_lossy();
        format!("'{}'", path.replace('\'', "'\"'\"'"))
    }

    pub fn managed_block(shell: Shell, path: &Path) -> Vec<u8> {
        let assignment = match shell {
            Shell::Zsh | Shell::Bash => format!("export CODEX_CLI_PATH={}", quote_path(path)),
            Shell::Fish => format!("set -gx CODEX_CLI_PATH {}", quote_path(path)),
        };
        format!(
            "# >>> flowdex managed CODEX_CLI_PATH >>>\n{assignment}\n# <<< flowdex managed CODEX_CLI_PATH <<<"
        )
        .into_bytes()
    }

    fn marker_positions(contents: &[u8], marker: &[u8]) -> Vec<usize> {
        let mut positions = Vec::new();
        let mut offset = 0;
        while let Some(relative) = contents[offset..]
            .windows(marker.len())
            .position(|window| window == marker)
        {
            let position = offset + relative;
            positions.push(position);
            offset = position + marker.len();
        }
        positions
    }

    fn line_start(contents: &[u8], position: usize) -> usize {
        contents[..position]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1)
    }

    fn line_end(contents: &[u8], position: usize) -> usize {
        contents[position..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(contents.len(), |index| position + index + 1)
    }

    pub fn replace_managed_block(contents: &[u8], block: &[u8]) -> Result<Vec<u8>> {
        let starts = marker_positions(contents, START_MARKER);
        let ends = marker_positions(contents, END_MARKER);
        if starts.len() > 1 || ends.len() > 1 {
            anyhow::bail!("profile contains duplicate Flowdex CODEX_CLI_PATH markers");
        }
        if starts.is_empty() != ends.is_empty() {
            anyhow::bail!("profile contains malformed Flowdex CODEX_CLI_PATH markers");
        }
        if starts.is_empty() {
            if block.is_empty() {
                return Ok(contents.to_vec());
            }
            let mut result = contents.to_vec();
            if !result.is_empty() && !result.ends_with(b"\n") {
                result.push(b'\n');
            }
            result.extend_from_slice(block);
            return Ok(result);
        }

        let start = starts[0];
        let end = ends[0];
        let start_line = line_start(contents, start);
        let start_line_end = line_end(contents, start);
        let end_line_start = line_start(contents, end);
        let end_line = line_end(contents, end + END_MARKER.len());
        if start != start_line
            || end < start
            || trim_line(&contents[start..start_line_end]) != START_MARKER
            || trim_line(&contents[end_line_start..end_line]) != END_MARKER
        {
            anyhow::bail!("profile contains malformed Flowdex CODEX_CLI_PATH markers");
        }
        let mut result = Vec::with_capacity(contents.len() + block.len());
        result.extend_from_slice(&contents[..start_line]);
        result.extend_from_slice(block);
        if !block.is_empty() && end_line > end + END_MARKER.len() {
            result.push(b'\n');
        }
        result.extend_from_slice(&contents[end_line..]);
        Ok(result)
    }

    pub fn remove_managed_block(contents: &[u8]) -> Result<Vec<u8>> {
        replace_managed_block(contents, b"")
    }

    pub fn install_profile_with_writer<F>(
        shell: Shell,
        profile: &Path,
        canonical: &Path,
        existing: &[u8],
        writer: F,
    ) -> Result<()>
    where
        F: FnOnce(&Path, &[u8]) -> Result<()>,
    {
        let contents = replace_managed_block(existing, &managed_block(shell, canonical))?;
        writer(profile, &contents)
    }

    fn trim_line(line: &[u8]) -> &[u8] {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        line.strip_suffix(b"\r").unwrap_or(line)
    }

    #[cfg(target_os = "macos")]
    pub fn write_profile_atomic(path: &Path, contents: &[u8]) -> Result<()> {
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
                ".flowdex-profile-{}-{nonce}-{attempt}",
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
                Err(error) => return Err(error).context("cannot create temporary profile"),
            }
        }
        let temp = temp.ok_or_else(|| anyhow::anyhow!("cannot allocate temporary profile"))?;
        let result = (|| {
            let mut file = file.take().expect("temporary profile file");
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
            file.write_all(contents)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, path)
                .with_context(|| format!("cannot replace profile {}", path.display()))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    #[cfg(target_os = "macos")]
    pub fn install(canonical: &Path) -> Result<()> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot locate the login profile"))?;
        let shell = std::env::var("SHELL").map_err(|_| anyhow::anyhow!("SHELL is not set"))?;
        let (shell, profile) = shell_and_profile(&shell, &home)?;
        let existing = match fs::read(&profile) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", profile.display()));
            }
        };
        install_profile_with_writer(shell, &profile, canonical, &existing, write_profile_atomic)?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub fn uninstall() -> Result<()> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot locate the login profile"))?;
        let shell_name = std::env::var("SHELL").map_err(|_| anyhow::anyhow!("SHELL is not set"))?;
        let (shell, profile) = shell_and_profile(&shell_name, &home)?;
        let existing = match fs::read(&profile) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", profile.display()));
            }
        };
        let contents = remove_managed_block(&existing)?;
        if contents == existing {
            return Ok(());
        }
        if shell == Shell::Fish && contents.is_empty() {
            fs::remove_file(&profile)?;
        } else {
            write_profile_atomic(&profile, &contents)?;
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn maps_supported_shells() {
            let home = Path::new("/Users/test");
            assert_eq!(shell_and_profile("/bin/zsh", home).unwrap().0, Shell::Zsh);
            assert_eq!(
                shell_and_profile("/bin/bash", home).unwrap().1,
                home.join(".bash_profile")
            );
            assert_eq!(
                shell_and_profile("/usr/bin/fish", home).unwrap().1,
                home.join(".config/fish/conf.d/flowdex.fish")
            );
            assert!(shell_and_profile("/bin/tcsh", home).is_err());
            assert!(shell_and_profile("/bin/zsh", Path::new("relative-home")).is_err());
        }

        #[test]
        fn quotes_path_as_data() {
            let block =
                String::from_utf8(managed_block(Shell::Zsh, Path::new("/tmp/a'b"))).unwrap();
            assert!(block.contains("export CODEX_CLI_PATH='/tmp/a'\"'\"'b'"));
            assert!(!block.contains(";"));
        }

        #[test]
        fn inserts_and_replaces_without_touching_unrelated_bytes() {
            let path = Path::new("/tmp/codex");
            let block = managed_block(Shell::Bash, path);
            let original = b"# keep this\n";
            let inserted = replace_managed_block(original, &block).unwrap();
            assert!(inserted.starts_with(original));
            let replaced = replace_managed_block(
                &inserted,
                &managed_block(Shell::Bash, Path::new("/tmp/new")),
            )
            .unwrap();
            assert!(replaced.starts_with(original));
            assert!(String::from_utf8(replaced).unwrap().contains("/tmp/new"));
            assert_eq!(remove_managed_block(&inserted).unwrap(), original);
        }

        #[test]
        fn rejects_malformed_and_duplicate_markers() {
            let block = managed_block(Shell::Fish, Path::new("/tmp/codex"));
            let duplicate = [block.as_slice(), b"\n", block.as_slice()].concat();
            assert!(replace_managed_block(&duplicate, &block).is_err());
            assert!(replace_managed_block(START_MARKER, &block).is_err());
        }

        #[test]
        fn profile_install_uses_injected_writer() {
            let profile = Path::new("/Users/test/.zprofile");
            let mut written = None;
            install_profile_with_writer(
                Shell::Zsh,
                profile,
                Path::new("/tmp/codex"),
                b"# unrelated\n",
                |path, contents| {
                    written = Some((path.to_path_buf(), contents.to_vec()));
                    Ok(())
                },
            )
            .unwrap();
            let (path, contents) = written.expect("writer called");
            assert_eq!(path, profile);
            assert!(contents.starts_with(b"# unrelated\n"));
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
    fn purge_removes_only_global_flowdex_data() {
        let codex_home = tempdir().expect("codex home");
        let data = codex_home.path().join("flowdex");
        fs::create_dir_all(data.join("workflows")).expect("workflow directory");
        fs::write(data.join("workflows").join("review.js"), b"workflow").expect("workflow");
        fs::write(codex_home.path().join("flowdex.toml"), b"config").expect("config");
        fs::write(codex_home.path().join("config.toml"), b"codex").expect("codex config");

        purge_flowdex_data(codex_home.path()).expect("purge");

        assert!(!data.exists());
        assert!(!codex_home.path().join("flowdex.toml").exists());
        assert_eq!(
            fs::read(codex_home.path().join("config.toml")).expect("codex config remains"),
            b"codex"
        );
    }
}
