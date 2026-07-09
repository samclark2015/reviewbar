//! Cross-platform command execution with PATH/env injection.
//!
//! GUI apps on macOS launch with a minimal PATH, and the original plugin worked
//! around this with hardcoded mise shims. Here `gh`/`git` (run directly) get
//! the user's per-repo `path_prepend` plus a set of common bin dirs prepended
//! to PATH. Build/launch commands, by contrast, run through the user's login
//! shell (see `default_shell`) so their shell rc is sourced and PATH resolves
//! exactly as it does in a real terminal — covering tools that live outside
//! mise, like rustup's `~/.cargo/bin`.

use std::collections::HashMap;
use std::path::Path;

use tokio::process::Command;

/// Result of a captured command run.
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// Expand a leading `~` to the user's home directory.
pub fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~") {
        if let Some(home) = home_dir() {
            return format!("{}{}", home, rest);
        }
    }
    p.to_string()
}

fn home_dir() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
}

/// Common bin dirs to ensure on PATH (after any user-supplied dirs).
fn base_path_dirs() -> Vec<String> {
    let mut dirs: Vec<String> = Vec::new();
    if cfg!(target_os = "windows") {
        // Inherited PATH on Windows is usually complete; nothing to add.
    } else {
        for d in [
            "~/.local/bin",
            "~/.local/share/mise/shims",
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
        ] {
            dirs.push(expand_tilde(d));
        }
    }
    dirs
}

fn path_sep() -> char {
    if cfg!(target_os = "windows") {
        ';'
    } else {
        ':'
    }
}

/// Apply env vars + a computed PATH (user dirs, then base dirs, then inherited).
fn apply_env(cmd: &mut Command, env: &HashMap<String, String>, path_prepend: &[String]) {
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut parts: Vec<String> = path_prepend.iter().map(|d| expand_tilde(d)).collect();
    parts.extend(base_path_dirs());
    if let Ok(existing) = std::env::var("PATH") {
        parts.push(existing);
    }
    cmd.env("PATH", parts.join(&path_sep().to_string()));
}

/// Run a binary directly (no shell), capturing stdout/stderr. Used for `gh`/`git`.
pub async fn run_program(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    env: &HashMap<String, String>,
    path_prepend: &[String],
) -> std::io::Result<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    apply_env(&mut cmd, env, path_prepend);
    let out = cmd.output().await?;
    Ok(Output {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

/// Open an interactive terminal in `cwd` running `command`.
///
/// Unlike build commands (which stream to the in-app log), interactive tools
/// like Claude Code need a real terminal. The terminal uses the user's login
/// shell, so PATH-managed tools (mise, node, claude) resolve as they do in a
/// normal terminal. Best-effort per OS.
pub fn open_terminal(cwd: &std::path::Path, command: &str) -> std::io::Result<()> {
    let dir = cwd.to_string_lossy().to_string();

    #[cfg(target_os = "macos")]
    {
        let shell_cmd = format!("cd {} && {}", quote(&dir), command);
        let esc = shell_cmd.replace('\\', "\\\\").replace('"', "\\\"");
        // Prefer iTerm if installed, otherwise the always-present Terminal.app.
        let script = if std::path::Path::new("/Applications/iTerm.app").exists() {
            format!(
                "tell application \"iTerm\"\n  activate\n  set w to (create window with default profile)\n  tell current session of w\n    write text \"{esc}\"\n  end tell\nend tell"
            )
        } else {
            format!("tell application \"Terminal\"\n  activate\n  do script \"{esc}\"\nend tell")
        };
        std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .spawn()?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        // `start "" cmd /K` opens a new console window that stays open.
        let inner = format!("cd /d \"{dir}\" && {command}");
        std::process::Command::new("cmd")
            .args(["/C", "start", "", "cmd", "/K", &inner])
            .spawn()?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Keep the window open after the command exits via `exec $SHELL`.
        let inner = format!("cd {} && {}; exec \"${{SHELL:-bash}}\"", quote(&dir), command);
        let candidates: [(&str, Vec<&str>); 4] = [
            ("gnome-terminal", vec!["--", "bash", "-lc", &inner]),
            ("konsole", vec!["-e", "bash", "-lc", &inner]),
            ("xterm", vec!["-e", "bash", "-lc", &inner]),
            ("x-terminal-emulator", vec!["-e", "bash", "-lc", &inner]),
        ];
        for (term, args) in candidates {
            if std::process::Command::new(term).args(&args).spawn().is_ok() {
                return Ok(());
            }
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no supported terminal emulator found",
        ));
    }

    #[allow(unreachable_code)]
    Ok(())
}

/// Quote a single argument for safe interpolation into a shell command string.
pub fn quote(arg: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        shlex::try_quote(arg)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| arg.to_string())
    }
}

/// Default shell invocation parts for the current OS.
///
/// On Unix we run the user's login shell (`$SHELL`) as a *login + interactive*
/// shell so their shell startup files are sourced — the same as a real
/// terminal. This matters because tools are often put on PATH by rc files
/// rather than by mise shims: e.g. rustup's `~/.cargo/bin` is added via
/// `. "$HOME/.cargo/env"` in `~/.zshrc`, and `mise activate` typically lives
/// there too. `.zshrc`/`.bashrc` are only sourced for *interactive* shells, so
/// a login-only shell (`-lc`) would still miss them; hence `-l -i -c`. Callers
/// must give the child no stdin (`Stdio::null()`), which `shell_command` does,
/// so the interactive shell can't block waiting for input.
fn default_shell() -> Vec<String> {
    if cfg!(target_os = "windows") {
        return vec!["cmd".into(), "/C".into()];
    }
    match std::env::var("SHELL") {
        Ok(sh) if !sh.trim().is_empty() => {
            vec![sh, "-l".into(), "-i".into(), "-c".into()]
        }
        _ => vec!["sh".into(), "-c".into()],
    }
}

/// Build a `Command` that runs `script` through a shell, with env/PATH applied.
/// stdout/stderr are left for the caller to configure (piped for streaming).
pub fn shell_command(
    script: &str,
    cwd: &Path,
    env: &HashMap<String, String>,
    path_prepend: &[String],
    shell: &Option<String>,
) -> Command {
    let parts = shell
        .as_ref()
        .and_then(|s| shlex::split(s))
        .filter(|p| !p.is_empty())
        .unwrap_or_else(default_shell);

    let mut cmd = Command::new(&parts[0]);
    for a in &parts[1..] {
        cmd.arg(a);
    }
    cmd.arg(script);
    cmd.current_dir(cwd);
    // Give the shell no stdin so an interactive login shell (see
    // `default_shell`) never blocks waiting for input. Callers configure
    // stdout/stderr themselves.
    cmd.stdin(std::process::Stdio::null());
    apply_env(&mut cmd, env, path_prepend);
    cmd
}
