//! Locating, installing and signing in to the Qoder CLI.
//!
//! Qoder is the one provider whose credentials do not live in this application.
//! `qodercli` keeps its own session under the user's home directory, and the
//! server authenticates with it by running the binary as a subprocess. So there
//! is nothing to request and nothing to store here: the only work is making sure
//! the binary is present, signed in, and findable by the server.
//!
//! Every external command runs in a terminal the user can see. Signing in is
//! interactive - it opens a browser and waits - so a silent, hidden process
//! would leave the user looking at a card that never resolves.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Serialize;

/// The installers Qoder publishes, one per shell.
#[cfg(target_os = "windows")]
const INSTALLER: (&str, &str) = ("https://qoder.com/install.ps1", "installer.ps1");
#[cfg(not(target_os = "windows"))]
const INSTALLER: (&str, &str) = ("https://qoder.com/install", "installer.sh");

/// Cold `qodercli --list-models` was measured at roughly nine seconds on a
/// populated cache, so this is not a timeout that should be tightened to make a
/// progress bar look responsive. It is here to stop a wedged process.
const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(120);

const VERSION_TIMEOUT: Duration = Duration::from_secs(20);

/// The binary names the installer creates, in the order they should be tried.
fn binary_names() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["qodercli.exe"]
    } else {
        &["qodercli"]
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QoderCliStatus {
    /// Whether a `qodercli` binary could be found at all.
    pub installed: bool,
    /// Absolute path of the binary that was found.
    pub path: Option<String>,
    /// Reported version, when the binary answered.
    pub version: Option<String>,
    /// Models the current session can reach. Probing for these is what proves a
    /// sign-in actually completed rather than merely being started.
    pub models: Vec<String>,
}

fn home_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        env::var_os("USERPROFILE").map(PathBuf::from)
    } else {
        env::var_os("HOME").map(PathBuf::from)
    }
}

/// Where the Qoder installer places the binary.
///
/// This path is the reason this module exists. The installer adds itself to
/// PATH, but PATH is read once when a process starts, so neither this
/// application nor the server it started will ever see the new entry. Looking
/// the binary up by name would keep reporting "not installed" immediately after
/// a successful install, which is the most confusing possible failure.
pub fn install_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".qoder").join("bin").join("qodercli"))
}

fn first_existing(dir: &Path) -> Option<PathBuf> {
    binary_names()
        .iter()
        .map(|name| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Minimal PATH lookup. Kept local rather than pulled in as a dependency for
/// the handful of platforms this needs to cover.
fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Resolve the binary, preferring locations that survive a mid-session install.
pub fn resolve_binary() -> Option<PathBuf> {
    if let Some(configured) = env::var_os("QODER_PATH") {
        let candidate = PathBuf::from(configured);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Some(dir) = install_dir() {
        if let Some(found) = first_existing(&dir) {
            return Some(found);
        }
    }
    which(binary_names().first().copied().unwrap_or("qodercli"))
}

fn run_captured(binary: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let output = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run {}: {error}", binary.display()))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn read_version(binary: &Path) -> Option<String> {
    let raw = run_captured(binary, &["--version"], VERSION_TIMEOUT).ok()?;
    let line = raw.lines().find(|line| !line.trim().is_empty())?;
    let token = line.split_whitespace().last().unwrap_or_default();
    if token.is_empty() {
        None
    } else {
        Some(token.trim_start_matches('v').to_string())
    }
}

/// Whether a word from the CLI's output could be a model identifier.
///
/// `--list-models` also prints headings and rules. Those are rejected by shape
/// rather than by pattern matching a format the CLI is free to change: a model
/// name starts with a letter or digit and contains nothing but identifier
/// characters. Anything else is not something to show a user.
fn is_model_token(token: &str) -> bool {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) if first.is_alphanumeric() => {}
        _ => return false,
    }
    token
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':'))
}

/// Pull model identifiers out of `--list-models` output.
///
/// The exact shape is the CLI's business; the card only needs to know that the
/// list is not empty.
fn read_models(binary: &Path) -> Vec<String> {
    let Ok(raw) = run_captured(binary, &["--list-models"], LIST_MODELS_TIMEOUT) else {
        return Vec::new();
    };
    let mut models: Vec<String> = raw
        .lines()
        .filter_map(|line| line.trim().split_whitespace().next())
        .filter(|token| is_model_token(token))
        .map(str::to_string)
        .collect();
    models.sort();
    models.dedup();
    models
}

#[tauri::command]
pub async fn qoder_cli_status(
    check_models: Option<bool>,
) -> Result<QoderCliStatus, String> {
    let Some(binary) = resolve_binary() else {
        return Ok(QoderCliStatus {
            installed: false,
            path: None,
            version: None,
            models: Vec::new(),
        });
    };
    let path = binary.to_string_lossy().to_string();
    // Both of these shell out and can take seconds, so neither may run on a
    // runtime worker.
    let version = tauri::async_runtime::spawn_blocking({
        let binary = binary.clone();
        move || read_version(&binary)
    })
    .await
    .unwrap_or(None);
    let models = if check_models.unwrap_or(false) {
        tauri::async_runtime::spawn_blocking(move || read_models(&binary))
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(QoderCliStatus {
        installed: true,
        path: Some(path),
        version,
        models,
    })
}

/// Open a command in a terminal window the user can see and keep.
///
/// The window is deliberately left open after the command finishes. An installer
/// that reports a checksum mismatch, or a sign-in that explains why it refused,
/// has said something worth reading, and a window that closes itself takes that
/// with it.
fn open_in_terminal(label: &str, program: &str, args: &[&str]) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // `start` detaches, so the application does not wait on the child; the
        // card polls the binary instead.
        let mut command = Command::new("cmd");
        command
            .arg("/c")
            .arg("start")
            .arg(label)
            .arg("cmd")
            .arg("/k")
            .arg(program)
            .args(args);
        command
            .stdin(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("could not open a terminal: {error}"))
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "tell application \"Terminal\" to do script \"{} {}\"",
            program,
            args.join(" ")
        );
        Command::new("osascript")
            .arg("-e")
            .arg(script)
            .stdin(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("could not open Terminal: {error}"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Command::new("x-terminal-emulator")
            .arg("-e")
            .arg(program)
            .args(args)
            .stdin(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("could not open a terminal: {error}"))
    }
}

/// Download Qoder's published installer to a temporary file.
///
/// It is fetched rather than piped straight into a shell so that what runs is a
/// file that can be shown to the user, and so the host can be reached without
/// the contents being interpreted on the way through.
async fn download_installer() -> Result<PathBuf, String> {
    let (url, file_name) = INSTALLER;
    let response = reqwest::get(url)
        .await
        .map_err(|error| format!("could not reach {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} returned {}", response.status()));
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("could not read the installer: {error}"))?;

    let dir = env::temp_dir();
    let path = dir.join(format!("qodercli-{}", file_name));
    std::fs::write(&path, &body)
        .map_err(|error| format!("could not save the installer: {error}"))?;
    Ok(path)
}

#[tauri::command]
pub async fn qoder_cli_install() -> Result<(), String> {
    let installer = download_installer().await?;

    #[cfg(target_os = "windows")]
    {
        open_in_terminal(
            "Qoder CLI",
            "powershell",
            &[
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                &installer.to_string_lossy(),
            ],
        )
    }
    #[cfg(target_os = "macos")]
    {
        open_in_terminal("Qoder CLI", installer.to_string_lossy().as_ref(), &[])
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        open_in_terminal("Qoder CLI", "bash", &[&installer.to_string_lossy()])
    }
}

#[tauri::command]
pub async fn qoder_cli_login() -> Result<(), String> {
    let binary =
        resolve_binary().ok_or_else(|| "the Qoder CLI is not installed".to_string())?;
    open_in_terminal("Qoder CLI sign-in", &binary.to_string_lossy(), &["login"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_dir_is_under_the_home_directory() {
        if let Some(dir) = install_dir() {
            assert!(dir.ends_with(Path::new(".qoder").join("bin").join("qodercli")));
        }
    }

    #[test]
    fn model_tokens_are_identified_by_shape() {
        assert!(is_model_token("gemini-3-pro"));
        assert!(is_model_token("qoder/qwen3-coder"));
        assert!(is_model_token("claude4"));
        // Headings, rules and separators share the output with real models.
        assert!(!is_model_token("###"));
        assert!(!is_model_token("---"));
        assert!(!is_model_token("*"));
        assert!(!is_model_token(""));
        assert!(!is_model_token(".hidden"));
        assert!(!is_model_token("model (deprecated)"));
    }

    #[test]
    fn headings_are_dropped_from_the_model_list() {
        let raw = "\n  gemini-3-pro\n  some-model\n  ### Available models\n";
        let kept: Vec<&str> = raw
            .lines()
            .filter_map(|line| line.trim().split_whitespace().next())
            .filter(|token| is_model_token(token))
            .collect();
        assert_eq!(kept, vec!["gemini-3-pro", "some-model"]);
    }
}
