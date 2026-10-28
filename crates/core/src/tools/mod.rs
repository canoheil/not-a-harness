//! Tool execution with file-system safety rails.
//!
//! Safety policy (the model is untrusted):
//! * every path is resolved *lexically* against the project root and must
//!   stay inside it (`..`, absolute paths and symlink escapes are rejected);
//! * destructive writes (`WRITE_FILE` over an existing file, `EDIT_FILE`)
//!   require an explicit confirmation carrying a preview diff, unless the
//!   user launched with auto-approve;
//! * tool output is size-capped so a huge file can't blow the context.

pub mod diff;

use std::fs;
use std::path::{Component, Path, PathBuf};

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

        // belt & braces: if the path exists, its canonical form must stay in root
        if let (Ok(real), Ok(real_root)) = (joined.canonicalize(), self.root.canonicalize()) {
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
        };
        match result {
            Ok(body) => ToolOutput {
                verb,
                path,
                body,
                is_error: false,
            },
            Err(body) => ToolOutput {
                verb,
                path,
                body,
                is_error: true,
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
            body.truncate(self.max_tool_bytes);
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
}
