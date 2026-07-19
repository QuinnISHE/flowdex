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
    #[cfg(not(windows))]
    {
        let _ = args;
        anyhow::bail!("`codex flowdex install` is only supported on Windows");
    }

    #[cfg(windows)]
    {
        let mut writer = RegistryEnvironmentWriter;
        let canonical = install_with_writer(args.binary, run_version_check, &mut writer)?;
        println!(
            "Configured CODEX_CLI_PATH={} for the current Windows user. Restart the Codex app to apply it.",
            canonical.display()
        );
        Ok(())
    }
}

trait EnvironmentWriter {
    fn set_codex_cli_path(&mut self, path: &Path) -> Result<()>;
}

fn install_with_writer<W, V>(
    binary: AbsolutePathBuf,
    version_check: V,
    writer: &mut W,
) -> Result<AbsolutePathBuf>
where
    W: EnvironmentWriter,
    V: FnOnce(&Path) -> Result<()>,
{
    let canonical = validate_binary(binary, version_check)?;
    writer.set_codex_cli_path(canonical.as_path())?;
    Ok(canonical)
}

fn validate_binary<V>(binary: AbsolutePathBuf, version_check: V) -> Result<AbsolutePathBuf>
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

    version_check(canonical.as_path())?;
    Ok(canonical)
}

#[cfg(windows)]
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

fn validate_version_output(success: bool, stdout: &[u8], stderr: &[u8]) -> Result<()> {
    if !success {
        anyhow::bail!("version command failed");
    }
    let mut output = String::from_utf8_lossy(stdout).to_ascii_lowercase();
    output.push_str(&String::from_utf8_lossy(stderr).to_ascii_lowercase());
    if !output.contains("codex") {
        anyhow::bail!("version output does not identify Codex");
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
        let error = validate_binary(non_exe, |_| {
            called.set(true);
            Ok(())
        })
        .expect_err("non-exe should fail");
        assert!(error.to_string().contains(".exe"));
        assert!(!called.get());

        let directory = AbsolutePathBuf::from_absolute_path(dir.path()).expect("absolute path");
        let error = validate_binary(directory, |_| Ok(())).expect_err("directory should fail");
        assert!(error.to_string().contains("regular file"));
    }

    #[test]
    fn missing_binary_fails_without_writer_mutation() {
        let dir = tempdir().expect("tempdir");
        let path = AbsolutePathBuf::from_absolute_path(dir.path().join("missing.exe"))
            .expect("absolute path");
        let writes = Cell::new(0);
        let mut writer = TestWriter { writes: &writes };
        let error = install_with_writer(path, |_| Ok(()), &mut writer).expect_err("missing");
        assert!(error.to_string().contains("canonicalize"));
        assert_eq!(writes.get(), 0);
    }

    #[test]
    fn version_output_must_succeed_and_identify_codex() {
        assert!(validate_version_output(true, b"codex 1.2.3", b"").is_ok());
        assert!(validate_version_output(false, b"codex 1.2.3", b"").is_err());
        assert!(validate_version_output(true, b"other 1.2.3", b"").is_err());
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
        )
        .expect("install");
        assert!(canonical.as_path().is_absolute());
        assert_eq!(writes.get(), 1);
    }
}
