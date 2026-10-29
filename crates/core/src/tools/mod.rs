//! Tool execution with file-system safety rails.
//!
//! Safety policy (the model is untrusted):
//! * every path is resolved *lexically* against the project root and must
//!   stay inside it (`..`, absolute paths and symlink escapes are rejected);
//! * destructive writes (`WRITE_FILE` over an existing file, `EDIT_FILE`)
//!   require an explicit confirmation carrying a preview diff, unless the
//!   user launched with auto-approve;
//! * `RUN_COMMAND` always shows the command for confirmation first, runs
//!   it in the project root with a hard timeout, and caps its output;
//! * tool output is size-capped so a huge file can't blow the context.

pub mod diff;

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::CoreError;
use crate::protocol::Action;

/// What the harness wants the user to confirm before it touches a file.
pub struct ConfirmRequest {
    pub verb: &'static str,
    pub path: String,
    pub preview: String,
    pub is_new_file: bool,
}

/// A tool result destined for the model (and the log).
pub struct ToolOutput {
    pub verb: &'static str,
    pub path: String,
    pub body: String,
    pub is_error: bool,
    /// True when the tool actually mutated the filesystem (used by the
    /// session's duplicate-action loop breaker).
    pub changed: bool,
}

impl ToolOutput {
    /// The wire format fed back to the model.
    pub fn to_model_message(&self) -> String {
        format!(
            "[tool result] {} {} ({})\n--- output ---\n{}\n--- end ---",
            self.verb,
            self.path,
            if self.is_error { "error" } else { "ok" },
            self.body
        )
    }
}

pub struct ToolKit {
    root: PathBuf,
    max_tool_bytes: usize,
}

impl ToolKit {
    pub fn new(root: PathBuf, max_tool_bytes: usize) -> ToolKit {
        ToolKit {
            root,
            max_tool_bytes,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a model-supplied relative path inside the project root.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, CoreError> {
        let rel = rel.trim();
        if rel.is_empty() {
            return Err(CoreError::Tool("empty path".into()));
        }
        let candidate = Path::new(rel);
        if candidate.is_absolute() {
            return Err(CoreError::Tool(format!(
                "absolute path rejected: '{rel}' (use paths relative to the project root)"
            )));
        }
        let mut clean = PathBuf::new();
        for comp in candidate.components() {
            match comp {
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(CoreError::Tool(format!(
                        "'..' is not allowed in paths: '{rel}'"
                    )));
                }
                Component::Normal(part) => clean.push(part),
                Component::RootDir | Component::Prefix(_) => {
                    return Err(CoreError::Tool(format!("bad path: '{rel}'")));
                }
            }
        }
        if clean.as_os_str().is_empty() {
            clean.push(".");
        }
        let joined = self.root.join(&clean);

        // belt & braces: the final path - or, for files that do not exist
        // yet, their nearest existing ancestor - must stay inside the root,
        // even through symlinked directories.
        let mut probe = joined.as_path();
        let real = loop {
            match probe.canonicalize() {
                Ok(real) => break Some(real),
                Err(_) => match probe.parent() {
                    Some(parent) => probe = parent,
                    None => break None,
                },
            }
        };
        if let (Some(real), Ok(real_root)) = (real, self.root.canonicalize()) {
            if !real.starts_with(&real_root) {
                return Err(CoreError::Tool(format!(
                    "path escapes the project root via a symlink: '{rel}'"
                )));
            }
        }
        Ok(joined)
    }

    pub fn run(
        &self,
        action: &Action,
        confirm: &mut dyn FnMut(&ConfirmRequest) -> bool,
    ) -> ToolOutput {
        let verb = action.verb();
        let path = action.path().to_string();
        let result = match action {
            Action::ReadFile {
                path,
                offset,
                limit,
            } => self.read_file(path, *offset, *limit).map_err(err_string),
            Action::WriteFile { path, content } => {
                self.write_file(path, content, confirm).map_err(err_string)
            }
            Action::EditFile {
                path,
                search,
                replace,
            } => self
                .edit_file(path, search, replace, confirm)
                .map_err(err_string),
            Action::ListDir { path } => self.list_dir(path).map_err(err_string),
            Action::RunCommand {
                command,
                cwd,
                timeout,
            } => self
                .run_command(command, cwd.as_deref(), timeout.unwrap_or(30), confirm)
                .map_err(err_string),
        };
        match result {
            Ok(body) => {
                let changed = matches!(action, Action::WriteFile { .. } | Action::EditFile { .. })
                    && !body.contains("unchanged");
                ToolOutput {
                    verb,
                    path,
                    body,
                    is_error: false,
                    changed,
                }
            }
            Err(body) => ToolOutput {
                verb,
                path,
                body,
                is_error: true,
                changed: false,
            },
        }
    }

    // -----------------------------------------------------------------

    fn read_file(
        &self,
        rel: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, CoreError> {
        let full = self.resolve(rel)?;
        let raw =
            fs::read(&full).map_err(|e| CoreError::Tool(format!("cannot read '{rel}': {e}")))?;
        let text = String::from_utf8_lossy(&raw).replace("\r\n", "\n");

        let start = offset.unwrap_or(1).max(1) - 1;
        let all: Vec<&str> = text.lines().collect();
        let end = match limit {
            Some(l) => (start + l).min(all.len()),
            None => all.len(),
        };
        if start >= all.len() && !all.is_empty() {
            return Err(CoreError::Tool(format!(
                "offset {offset:?} is past the end of '{rel}' ({} lines)",
                all.len()
            )));
        }
        let slice = &all[start..end];

        let mut body = slice.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        let mut header = format!(
            "// {} — lines {}..{} of {}\n",
            rel,
            start + 1,
            end,
            all.len()
        );
        if body.len() > self.max_tool_bytes {
            // truncate on a char boundary - panics otherwise on multibyte text
            let mut cut = self.max_tool_bytes.min(body.len());
            while cut > 0 && !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
            body.push_str("\n// ... truncated by harness (use offset:/limit: to read more)\n");
        } else if end < all.len() {
            header.push_str(&format!(
                "// ... more lines below — use offset: {} to continue\n",
                end + 1
            ));
        }
        Ok(format!("{header}{body}"))
    }

    fn write_file(
        &self,
        rel: &str,
        content: &str,
        confirm: &mut dyn FnMut(&ConfirmRequest) -> bool,
    ) -> Result<String, CoreError> {
        let full = self.resolve(rel)?;
        let content = content.replace("\r\n", "\n");
        let existed = full.exists();
        if existed {
            let old =
                String::from_utf8_lossy(&fs::read(&full).unwrap_or_default()).replace("\r\n", "\n");
            if old == content {
                return Ok(format!("'{rel}' unchanged (content identical)."));
            }
            let preview = diff::render(&old, &content);
            let approved = confirm(&ConfirmRequest {
                verb: "WRITE_FILE",
                path: rel.to_string(),
                preview,
                is_new_file: false,
            });
            if !approved {
                return Err(CoreError::Tool(format!(
                    "write to '{rel}' was rejected by the user"
                )));
            }
        } else {
            let approved = confirm(&ConfirmRequest {
                verb: "WRITE_FILE",
                path: rel.to_string(),
                preview: diff::render("", &content),
                is_new_file: true,
            });
            if !approved {
                return Err(CoreError::Tool(format!(
                    "creation of '{rel}' was rejected by the user"
                )));
            }
        }
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CoreError::Tool(format!("cannot create dir: {e}")))?;
        }
        fs::write(&full, &content)
            .map_err(|e| CoreError::Tool(format!("cannot write '{rel}': {e}")))?;
        Ok(format!(
            "wrote '{}' ({} lines).",
            rel,
            content.lines().count()
        ))
    }

    fn edit_file(
        &self,
        rel: &str,
        search: &str,
        replace: &str,
        confirm: &mut dyn FnMut(&ConfirmRequest) -> bool,
    ) -> Result<String, CoreError> {
        let full = self.resolve(rel)?;
        let old =
            String::from_utf8_lossy(&fs::read(&full).unwrap_or_default()).replace("\r\n", "\n");
        if search.is_empty() {
            return Err(CoreError::Tool("SEARCH text is empty".into()));
        }
        let hits = old.matches(search).count();
        if hits == 0 {
            return Err(CoreError::Tool(format!(
                "SEARCH text not found in '{rel}'. READ_FILE the current content first \
                 and retry with an exact match (whitespace matters)."
            )));
        }
        if hits > 1 {
            return Err(CoreError::Tool(format!(
                "SEARCH text matches {hits} places in '{rel}' — add more context lines \
                 to make it unique."
            )));
        }
        let new = old.replacen(search, replace, 1);
        let preview = diff::render(&old, &new);
        let approved = confirm(&ConfirmRequest {
            verb: "EDIT_FILE",
            path: rel.to_string(),
            preview,
            is_new_file: false,
        });
        if !approved {
            return Err(CoreError::Tool(format!(
                "edit of '{rel}' was rejected by the user"
            )));
        }
        fs::write(&full, &new)
            .map_err(|e| CoreError::Tool(format!("cannot write '{rel}': {e}")))?;
        Ok(format!("edited '{rel}' in one place."))
    }

    fn list_dir(&self, rel: &str) -> Result<String, CoreError> {
        let full = self.resolve(rel)?;
        if !full.is_dir() {
            return Err(CoreError::Tool(format!("'{rel}' is not a directory")));
        }
        const SKIP: &[&str] = &[
            ".git",
            "target",
            "node_modules",
            "__pycache__",
            ".venv",
            "dist",
            "build",
        ];
        const MAX_ENTRIES: usize = 200;

        let mut out = String::new();
        let mut count = 0usize;
        walk(&full, 0, &mut out, &mut count, SKIP, MAX_ENTRIES);
        if count >= MAX_ENTRIES {
            out.push_str("... (listing truncated)\n");
        }
        if out.is_empty() {
            out.push_str("(empty directory)\n");
        }
        Ok(out)
    }

    /// Execute a shell command inside the project root. Always shows the
    /// command for confirmation first (or launch-time auto-approve); the
    /// process is killed on timeout and its output is size-capped.
    fn run_command(
        &self,
        command: &str,
        cwd: Option<&str>,
        timeout_s: u32,
        confirm: &mut dyn FnMut(&ConfirmRequest) -> bool,
    ) -> Result<String, CoreError> {
        let timeout_s = timeout_s.clamp(1, 300);
        let dir = match cwd {
            Some(d) => {
                let p = self.resolve(d)?;
                if !p.is_dir() {
                    return Err(CoreError::Tool(format!("'{d}' is not a directory")));
                }
                p
            }
            None => self.root.clone(),
        };
        #[cfg(windows)]
        let shell_desc = "cmd /C";
        #[cfg(not(windows))]
        let shell_desc = "sh -c";

        let mut preview = format!(
            "shell: {shell_desc}\nruns in: {}\ntimeout: {timeout_s}s",
            crate::tools::display_path(&dir)
        );
        if looks_destructive(command) {
            preview.push_str("\n! warning: this command looks destructive");
        }
        let request = ConfirmRequest {
            verb: "RUN_COMMAND",
            path: command.to_string(),
            preview,
            is_new_file: false,
        };
        if !confirm(&request) {
            return Err(CoreError::Tool(format!(
                "command '{command}' was rejected by the user"
            )));
        }

        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.arg("/C");
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.arg("-c");
            c
        };
        cmd.arg(command).current_dir(&dir).stdin(Stdio::null());

        // Capture output via temp files, not pipes: a killed shell's
        // grandchildren must never block us waiting for pipe EOF. Names
        // are unique per call (atomic counter) so concurrent runs - even
        // inside one process, as tests do - cannot collide.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = format!("{}_{}", std::process::id(), n);
        let out_path = std::env::temp_dir().join(format!("nah_cmd_out_{stamp}"));
        let err_path = std::env::temp_dir().join(format!("nah_cmd_err_{stamp}"));
        let out_file =
            fs::File::create(&out_path).map_err(|e| CoreError::Tool(format!("temp file: {e}")))?;
        let err_file =
            fs::File::create(&err_path).map_err(|e| CoreError::Tool(format!("temp file: {e}")))?;
        cmd.stdout(Stdio::from(out_file));
        cmd.stderr(Stdio::from(err_file));

        // run in its own process group (unix) so a timeout can kill the
        // whole tree, not just the shell
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    if setpgid(0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| CoreError::Tool(format!("cannot start command: {e}")))?;

        let deadline = Instant::now() + Duration::from_secs(timeout_s as u64);
        let mut timed_out = false;
        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        timed_out = true;
                        kill_tree(&mut child);
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => {
                    kill_tree(&mut child);
                    break None;
                }
            }
        };
        let out_bytes = fs::read(&out_path).unwrap_or_default();
        let err_bytes = fs::read(&err_path).unwrap_or_default();
        let out_text = String::from_utf8_lossy(&out_bytes);
        let err_text = String::from_utf8_lossy(&err_bytes);
        let _ = fs::remove_file(&out_path);
        let _ = fs::remove_file(&err_path);

        let mut body = if timed_out {
            format!("timed out after {timeout_s}s (process killed)\n")
        } else {
            match code {
                Some(c) => format!("exit code: {c}\n"),
                None => "terminated abnormally\n".to_string(),
            }
        };
        body.push_str("--- stdout ---\n");
        body.push_str(&out_text);
        if !out_text.is_empty() && !out_text.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("--- stderr ---\n");
        body.push_str(&err_text);
        if body.len() > self.max_tool_bytes {
            // truncate on a char boundary - panics otherwise on multibyte text
            let mut cut = self.max_tool_bytes.min(body.len());
            while cut > 0 && !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
            body.push_str("\n... truncated by harness");
        }
        Ok(body)
    }
}

/// Human-facing path display: strips the Windows verbatim prefix
/// (`\\?\C:\...`) that `canonicalize()` leaves in.
pub fn display_path(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => s,
    }
}

/// Kill a spawned command and everything it spawned. On unix the child
/// runs in its own process group (set in pre_exec), so one signal to the
/// group takes the whole tree; on Windows `taskkill /T` walks the tree.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        // SIGKILL to the child's process group (pgid == child pid)
        kill(-(child.id() as i32), 9);
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
unsafe extern "C" {
    fn setpgid(pid: i32, pgid: i32) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Heuristic for the confirmation dialog - display only, not a security
/// boundary (the confirmation itself is the gate).
fn looks_destructive(command: &str) -> bool {
    let lower = command.to_lowercase();
    [
        "rm -rf",
        "rm -fr",
        "rm -r /",
        "del /f",
        "rd /s",
        "format c",
        "mkfs",
        "dd if=",
        "> /dev/sd",
        "reg delete",
        ":(){",
        "shutdown",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

fn err_string(e: CoreError) -> String {
    e.to_string()
}

fn walk(dir: &Path, depth: usize, out: &mut String, count: &mut usize, skip: &[&str], max: usize) {
    if depth > 3 || *count >= max {
        return;
    }
    let mut entries: Vec<(bool, String, u64)> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if skip.contains(&name.as_str()) {
                    return None;
                }
                let meta = e.metadata().ok()?;
                Some((meta.is_dir(), name, meta.len()))
            })
            .collect(),
        Err(_) => return,
    };
    entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    for (is_dir, name, size) in entries {
        if *count >= max {
            return;
        }
        *count += 1;
        let indent = "  ".repeat(depth);
        if is_dir {
            out.push_str(&format!("{indent}{name}/\n"));
            let child = dir.join(&name);
            walk(&child, depth + 1, out, count, skip, max);
        } else {
            out.push_str(&format!("{indent}{name} ({})\n", human_size(size)));
        }
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", UNITS[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kit(dir: &Path) -> ToolKit {
        ToolKit::new(dir.to_path_buf(), 4096)
    }

    fn auto(_: &ConfirmRequest) -> bool {
        true
    }

    #[test]
    fn path_sandbox() {
        let dir = std::env::temp_dir().join("nah_test_sandbox");
        fs::create_dir_all(&dir).unwrap();
        let k = kit(&dir);
        assert!(k.resolve("src/main.rs").is_ok());
        assert!(k.resolve("./a").is_ok());
        assert!(k.resolve("../evil").is_err());
        assert!(k.resolve("/etc/passwd").is_err());
        assert!(k.resolve("a/../../evil").is_err());
    }

    #[test]
    fn run_command_executes_and_reports() {
        let dir = std::env::temp_dir().join("nah_test_rc_exec");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let k = kit(&dir);
        let action = Action::RunCommand {
            command: "echo nah-test-ok".to_string(),
            cwd: None,
            timeout: Some(10),
        };
        let out = k.run(&action, &mut auto);
        assert!(!out.is_error, "{}", out.body);
        assert!(out.body.contains("exit code: 0"), "{}", out.body);
        assert!(out.body.contains("nah-test-ok"), "{}", out.body);
        assert!(!out.changed, "commands never count as file changes");
    }

    #[test]
    fn run_command_rejection_is_an_error() {
        let dir = std::env::temp_dir().join("nah_test_rc_reject");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let k = kit(&dir);
        let action = Action::RunCommand {
            command: "echo should-not-run".to_string(),
            cwd: None,
            timeout: Some(10),
        };
        let out = k.run(&action, &mut |_: &ConfirmRequest| false);
        assert!(out.is_error);
        assert!(out.body.contains("rejected by the user"), "{}", out.body);
    }

    #[test]
    fn run_command_cwd_is_sandboxed() {
        let dir = std::env::temp_dir().join("nah_test_rc_cwd");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let k = kit(&dir);
        let action = Action::RunCommand {
            command: "echo hi".to_string(),
            cwd: Some("../evil".to_string()),
            timeout: Some(10),
        };
        let out = k.run(&action, &mut auto);
        assert!(out.is_error, "{}", out.body);
    }

    #[cfg(unix)]
    #[test]
    fn run_command_timeout_kills() {
        let dir = std::env::temp_dir().join("nah_test_rc_timeout");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let k = kit(&dir);
        let action = Action::RunCommand {
            command: "sleep 5".to_string(),
            cwd: None,
            timeout: Some(1),
        };
        let started = std::time::Instant::now();
        let out = k.run(&action, &mut auto);
        assert!(
            started.elapsed().as_secs() < 4,
            "kill must not wait for sleep"
        );
        assert!(!out.is_error, "{}", out.body);
        assert!(out.body.contains("timed out"), "{}", out.body);
    }

    #[test]
    fn write_read_edit_cycle() {
        let dir = std::env::temp_dir().join("nah_test_cycle");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let k = kit(&dir);

        let w = k.run(
            &Action::WriteFile {
                path: "a/b.txt".into(),
                content: "hello\nworld\n".into(),
            },
            &mut auto,
        );
        assert!(!w.is_error, "{}", w.body);

        let r = k.run(
            &Action::ReadFile {
                path: "a/b.txt".into(),
                offset: None,
                limit: None,
            },
            &mut auto,
        );
        assert!(!r.is_error);
        assert!(r.body.contains("hello"));

        let e = k.run(
            &Action::EditFile {
                path: "a/b.txt".into(),
                search: "hello".into(),
                replace: "hi".into(),
            },
            &mut auto,
        );
        assert!(!e.is_error, "{}", e.body);
        let now = fs::read_to_string(dir.join("a/b.txt")).unwrap();
        assert_eq!(now, "hi\nworld\n");
    }

    #[test]
    fn edit_requires_unique_match() {
        let dir = std::env::temp_dir().join("nah_test_unique");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let k = kit(&dir);
        fs::write(dir.join("x.txt"), "aa\naa\n").unwrap();
        let e = k.run(
            &Action::EditFile {
                path: "x.txt".into(),
                search: "aa".into(),
                replace: "bb".into(),
            },
            &mut auto,
        );
        assert!(e.is_error);
        assert!(e.body.contains("2 places"));
    }

    #[test]
    fn write_rejection_is_respected() {
        let dir = std::env::temp_dir().join("nah_test_reject");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let k = kit(&dir);
        fs::write(dir.join("x.txt"), "old").unwrap();
        let w = k.run(
            &Action::WriteFile {
                path: "x.txt".into(),
                content: "new".into(),
            },
            &mut |_: &ConfirmRequest| false,
        );
        assert!(w.is_error);
        assert_eq!(fs::read_to_string(dir.join("x.txt")).unwrap(), "old");
    }

    #[test]
    fn read_truncation_and_limits() {
        let dir = std::env::temp_dir().join("nah_test_trunc");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let k = ToolKit::new(dir.clone(), 20);
        fs::write(dir.join("big.txt"), "a\nb\nc\nd\ne\n").unwrap();
        let r = k.run(
            &Action::ReadFile {
                path: "big.txt".into(),
                offset: Some(2),
                limit: Some(2),
            },
            &mut auto,
        );
        assert!(r.body.contains("lines 2..3 of 5"));
        assert!(r.body.contains('b'));
        assert!(r.body.contains('c'));
        assert!(!r.body.contains('a'));
    }

    #[test]
    fn truncation_is_multibyte_safe() {
        // regression: String::truncate panics when the byte limit lands
        // inside a multibyte char (very common with Persian content)
        let dir = std::env::temp_dir().join("nah_test_trunc_utf8");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // max_tool_bytes=5 lands in the middle of the 2-byte 'س'
        let k = ToolKit::new(dir.clone(), 5);
        fs::write(dir.join("fa.txt"), "سلام دنیا\n").unwrap();
        let out = k.run(
            &Action::ReadFile {
                path: "fa.txt".into(),
                offset: None,
                limit: None,
            },
            &mut auto,
        );
        assert!(!out.is_error, "{}", out.body);
        assert!(out.body.contains("truncated"), "{}", out.body);
    }

    #[test]
    fn write_through_symlinked_parent_is_blocked() {
        let base = std::env::temp_dir().join("nah_test_symlink");
        let _ = fs::remove_dir_all(&base);
        let root = base.join("root");
        let outside = base.join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        // root/link -> outside (symlink). Windows may lack symlink perms,
        // so skip silently if we cannot create one.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            if symlink(&outside, root.join("link")).is_err() {
                return;
            }
            let k = kit(&root);
            // the file does not exist yet - the old check missed this case
            let w = k.run(
                &Action::WriteFile {
                    path: "link/evil.txt".into(),
                    content: "x".into(),
                },
                &mut auto,
            );
            assert!(w.is_error, "symlink escape must be rejected");
            assert!(!outside.join("evil.txt").exists());
        }
    }

    #[test]
    fn display_path_strips_windows_verbatim_prefix() {
        assert_eq!(
            display_path(std::path::Path::new(r"\\?\C:\Users\x\proj")),
            r"C:\Users\x\proj"
        );
        // plain paths are untouched
        assert_eq!(display_path(std::path::Path::new("/home/x")), "/home/x");
    }
}
