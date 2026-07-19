use anyhow::Context;
use anyhow::Result;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::path::Path;

#[derive(Debug, clap::Args)]
pub struct FlowdexCli {
    #[command(subcommand)]
    pub subcommand: FlowdexSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum FlowdexSubcommand {
    /// Configure the Codex desktop app to use a local Codex binary.
    Install(InstallArgs),
}

#[derive(Debug, clap::Args)]
pub struct InstallArgs {
    /// Absolute path to the Codex executable.
    #[arg(long, value_parser = parse_absolute_binary_path)]
    pub binary: AbsolutePathBuf,
}

fn parse_absolute_binary_path(raw: &str) -> std::result::Result<AbsolutePathBuf, String> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err("--binary must be an absolute path".to_string());
    }
    AbsolutePathBuf::from_absolute_path_checked(path)
        .map_err(|error| format!("invalid --binary path: {error}"))
}

pub async fn run(cli: FlowdexCli) -> Result<()> {
    match cli.subcommand {
        FlowdexSubcommand::Install(args) => run_install(args).await,
    }
}

async fn run_install(args: InstallArgs) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        return macos::run(args).await;
    }

    #[cfg(target_os = "windows")]
    {
        let mut writer = RegistryEnvironmentWriter;
        let canonical = install_with_writer(
            args.binary,
            run_version_check,
            &mut writer,
            BinaryValidation::Windows,
        )?;
        println!(
            "Configured CODEX_CLI_PATH={} for the current Windows user. Restart the Codex app to apply it.",
            canonical.display()
        );
        Ok(())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = args;
        anyhow::bail!("`codex flowdex install` is only supported on Windows and macOS");
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
enum BinaryValidation {
    Windows,
    Macos,
}

trait EnvironmentWriter {
    fn set_codex_cli_path(&mut self, path: &Path) -> Result<()>;
}

fn install_with_writer<W, V>(
    binary: AbsolutePathBuf,
    version_check: V,
    writer: &mut W,
    validation: BinaryValidation,
) -> Result<AbsolutePathBuf>
where
    W: EnvironmentWriter,
    V: FnOnce(&Path) -> Result<()>,
{
    let canonical = validate_binary(binary, version_check, validation)?;
    writer.set_codex_cli_path(canonical.as_path())?;
    Ok(canonical)
}

fn validate_binary<V>(
    binary: AbsolutePathBuf,
    version_check: V,
    validation: BinaryValidation,
) -> Result<AbsolutePathBuf>
where
    V: FnOnce(&Path) -> Result<()>,
{
    let canonical = binary
        .canonicalize()
        .with_context(|| format!("cannot canonicalize --binary path: {}", binary.display()))?;
    let metadata = std::fs::metadata(canonical.as_path())
        .with_context(|| format!("cannot inspect --binary path: {}", canonical.display()))?;
    if !metadata.is_file() {
        anyhow::bail!(
            "--binary must point to a regular file: {}",
            canonical.display()
        );
    }
    if matches!(validation, BinaryValidation::Windows) {
        let is_exe = canonical
            .as_path()
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"));
        if !is_exe {
            anyhow::bail!(
                "--binary must point to a .exe file: {}",
                canonical.display()
            );
        }
    }
    if matches!(validation, BinaryValidation::Macos) {
        #[cfg(target_family = "unix")]
        {
            use std::os::unix::fs::PermissionsExt;
            if canonical.as_path().metadata()?.permissions().mode() & 0o111 == 0 {
                anyhow::bail!("--binary must be executable: {}", canonical.display());
            }
        }
    }

    version_check(canonical.as_path())?;
    Ok(canonical)
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
        if end_line > end + END_MARKER.len() {
            result.push(b'\n');
        }
        result.extend_from_slice(&contents[end_line..]);
        Ok(result)
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
    pub async fn run(args: super::InstallArgs) -> Result<()> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot locate the login profile"))?;
        let shell = std::env::var("SHELL").map_err(|_| anyhow::anyhow!("SHELL is not set"))?;
        let (shell, profile) = shell_and_profile(&shell, &home)?;
        let canonical = super::validate_binary(
            args.binary,
            super::run_version_check,
            super::BinaryValidation::Macos,
        )?;
        let existing = match fs::read(&profile) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", profile.display()));
            }
        };
        install_profile_with_writer(
            shell,
            &profile,
            canonical.as_path(),
            &existing,
            write_profile_atomic,
        )?;
        println!(
            "Configured CODEX_CLI_PATH={} in {}. Fully quit and restart the Codex app to apply it.",
            canonical.display(),
            profile.display()
        );
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
    let Some(version) = output.strip_prefix("codex ") else {
        anyhow::bail!("version output does not match `codex <version>`");
    };
    if version.is_empty()
        || version.chars().any(char::is_whitespace)
        || !version
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".+-".contains(character))
    {
        anyhow::bail!("version output does not match `codex <version>`");
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use tempfile::tempdir;

    struct TestWriter<'a> {
        writes: &'a Cell<usize>,
    }

    impl EnvironmentWriter for TestWriter<'_> {
        fn set_codex_cli_path(&mut self, _path: &Path) -> Result<()> {
            self.writes.set(self.writes.get() + 1);
            Ok(())
        }
    }

    fn binary_path(name: &str) -> (tempfile::TempDir, AbsolutePathBuf) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, b"binary").expect("binary");
        (
            dir,
            AbsolutePathBuf::from_absolute_path(path).expect("absolute path"),
        )
    }

    #[test]
    fn rejects_non_exe_and_directories_before_version_check() {
        let (dir, non_exe) = binary_path("codex");
        let called = Cell::new(false);
        let error = validate_binary(
            non_exe,
            |_| {
                called.set(true);
                Ok(())
            },
            BinaryValidation::Windows,
        )
        .expect_err("non-exe should fail");
        assert!(error.to_string().contains(".exe"));
        assert!(!called.get());

        let directory = AbsolutePathBuf::from_absolute_path(dir.path()).expect("absolute path");
        let error = validate_binary(directory, |_| Ok(()), BinaryValidation::Windows)
            .expect_err("directory should fail");
        assert!(error.to_string().contains("regular file"));
    }

    #[test]
    fn missing_binary_fails_without_writer_mutation() {
        let dir = tempdir().expect("tempdir");
        let path = AbsolutePathBuf::from_absolute_path(dir.path().join("missing.exe"))
            .expect("absolute path");
        let writes = Cell::new(0);
        let mut writer = TestWriter { writes: &writes };
        let error = install_with_writer(path, |_| Ok(()), &mut writer, BinaryValidation::Windows)
            .expect_err("missing");
        assert!(error.to_string().contains("canonicalize"));
        assert_eq!(writes.get(), 0);
    }

    #[test]
    fn version_output_must_succeed_and_identify_codex() {
        assert!(validate_version_output(true, b"codex 1.2.3", b"").is_ok());
        assert!(validate_version_output(true, b"codex 1.2.3\n", b"").is_ok());
        assert!(validate_version_output(false, b"codex 1.2.3", b"").is_err());
        assert!(validate_version_output(true, b"not a codex-compatible tool", b"").is_err());
        assert!(validate_version_output(true, b"", b"codex 1.2.3").is_err());
        assert!(validate_version_output(true, b"codex", b"").is_err());
    }

    #[test]
    fn successful_install_writes_only_after_validation() {
        let (_dir, path) = binary_path("codex.exe");
        let writes = Cell::new(0);
        let mut writer = TestWriter { writes: &writes };
        let canonical = install_with_writer(
            path,
            |validated| {
                assert!(validated.is_absolute());
                Ok(())
            },
            &mut writer,
            BinaryValidation::Windows,
        )
        .expect("install");
        assert!(canonical.as_path().is_absolute());
        assert_eq!(writes.get(), 1);
    }
}
