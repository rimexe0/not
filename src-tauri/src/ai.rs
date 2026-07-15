use crate::db::AiSettings;
use serde::Serialize;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub id: &'static str,
    pub display_name: &'static str,
    pub available: bool,
    pub executable: Option<String>,
}

pub fn detect_providers(settings: &AiSettings) -> Vec<ProviderStatus> {
    [
        ("claude", "Claude Code", resolve_executable("claude")),
        ("codex", "OpenAI Codex", resolve_executable("codex")),
        (
            "custom",
            "Custom command",
            resolve_configured_executable(&settings.custom_program),
        ),
    ]
    .into_iter()
    .map(|(id, display_name, executable)| ProviderStatus {
        id,
        display_name,
        available: executable.is_some(),
        executable: executable.map(|path| path.to_string_lossy().into_owned()),
    })
    .collect()
}

pub fn run_provider(
    provider: &str,
    action: &str,
    note: &str,
    settings: &AiSettings,
    cancelled: Arc<AtomicBool>,
) -> Result<String, String> {
    if note.trim().is_empty() {
        return Err("the current page is empty".to_string());
    }
    let instruction = match action {
        "summarize" => "Summarize these scratch notes concisely. Output only the summary.",
        "organize" => {
            "Organize these scratch notes as clear Markdown. Preserve every piece of information, do not invent content, and output only the reorganized note."
        }
        _ => return Err("unknown AI action".to_string()),
    };
    let prompt = format!("{instruction}\n\n<scratch-notes>\n{note}\n</scratch-notes>\n");
    let (program, arguments) = provider_command(provider, settings)?;
    let working_directory = tempfile::tempdir().map_err(error)?;
    let mut child = Command::new(program)
        .args(arguments)
        .current_dir(working_directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("NO_COLOR", "1")
        .spawn()
        .map_err(error)?;

    child
        .stdin
        .take()
        .ok_or_else(|| "AI provider stdin unavailable".to_string())?
        .write_all(prompt.as_bytes())
        .map_err(error)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "AI provider stdout unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "AI provider stderr unavailable".to_string())?;
    let stdout_reader = thread::spawn(move || read_all(stdout));
    let stderr_reader = thread::spawn(move || read_all(stderr));

    let started = Instant::now();
    let status = loop {
        if cancelled.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("AI request cancelled".to_string());
        }
        if started.elapsed() >= Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("AI provider timed out after 120 seconds".to_string());
        }
        if let Some(status) = child.try_wait().map_err(error)? {
            break status;
        }
        thread::sleep(Duration::from_millis(40));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "AI stdout reader failed".to_string())??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "AI stderr reader failed".to_string())??;
    if !status.success() {
        let message = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(if message.is_empty() {
            format!("AI provider exited with {status}")
        } else {
            message
        });
    }
    let output = String::from_utf8(stdout).map_err(error)?;
    let output = output.trim();
    if output.is_empty() {
        return Err("AI provider returned no text".to_string());
    }
    Ok(output.to_string())
}

fn provider_command(
    provider: &str,
    settings: &AiSettings,
) -> Result<(PathBuf, Vec<String>), String> {
    match provider {
        "claude" => Ok((
            resolve_executable("claude").ok_or_else(|| "Claude Code was not found".to_string())?,
            vec![
                "--print".to_string(),
                "--no-session-persistence".to_string(),
                "--tools".to_string(),
                "".to_string(),
            ],
        )),
        "codex" => Ok((
            resolve_executable("codex").ok_or_else(|| "Codex CLI was not found".to_string())?,
            vec![
                "exec".to_string(),
                "--ephemeral".to_string(),
                "--sandbox".to_string(),
                "read-only".to_string(),
                "--skip-git-repo-check".to_string(),
                "--ignore-rules".to_string(),
                "-".to_string(),
            ],
        )),
        "custom" => {
            let program = resolve_configured_executable(&settings.custom_program)
                .ok_or_else(|| "custom executable was not found".to_string())?;
            let arguments = shell_words::split(&settings.custom_arguments).map_err(error)?;
            Ok((program, arguments))
        }
        _ => Err("unknown AI provider".to_string()),
    }
}

fn resolve_configured_executable(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else if value.contains('/') {
        let path = PathBuf::from(value);
        is_executable_file(&path).then_some(path)
    } else {
        resolve_executable(value)
    }
}

fn resolve_executable(name: &str) -> Option<PathBuf> {
    let mut directories = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    directories.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        directories.extend([home.join(".local/bin"), home.join(".cargo/bin")]);
    }
    directories
        .into_iter()
        .map(|directory| directory.join(name))
        .find(|path| is_executable_file(path))
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

fn read_all(mut reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(error)?;
    Ok(bytes)
}

fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::{provider_command, run_provider};
    use crate::db::AiSettings;
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn custom_arguments_are_parsed_without_a_shell() {
        let settings = AiSettings {
            custom_program: "/usr/bin/printf".to_string(),
            custom_arguments: "'%s' hello".to_string(),
            last_provider: None,
        };
        let (program, arguments) = provider_command("custom", &settings).unwrap();
        assert_eq!(program.to_string_lossy(), "/usr/bin/printf");
        assert_eq!(arguments, ["%s", "hello"]);
    }

    #[test]
    fn custom_provider_receives_prompt_and_returns_output() {
        let settings = AiSettings {
            custom_program: "/bin/cat".to_string(),
            custom_arguments: String::new(),
            last_provider: None,
        };
        let result = run_provider(
            "custom",
            "summarize",
            "remember milk",
            &settings,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(result.contains("remember milk"));
        assert!(result.contains("Summarize"));
    }

    #[test]
    fn provider_can_be_cancelled() {
        let settings = AiSettings {
            custom_program: "/bin/cat".to_string(),
            custom_arguments: String::new(),
            last_provider: None,
        };
        let cancelled = Arc::new(AtomicBool::new(true));
        let result = run_provider("custom", "summarize", "private note", &settings, cancelled);
        assert_eq!(result.unwrap_err(), "AI request cancelled");
    }
}
