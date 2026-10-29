//! The text tool protocol: models emit fenced `tool` blocks instead of JSON.
//!
//! Grammar (line-oriented, tolerant — invalid blocks stay as prose):
//!
//! ````text
//! ```tool
//! READ_FILE
//! path: src/main.rs
//! ```
//!
//! ```tool
//! WRITE_FILE
//! path: docs/a.md
//! content:
//! ...anything, until a line that is exactly END_OF_CONTENT
//! END_OF_CONTENT
//! ```
//!
//! ```tool
//! EDIT_FILE
//! path: src/main.rs
//! <<<<<<< SEARCH
//! exact text to find
//! =======
//! replacement
//! >>>>>>> REPLACE
//! ```
//!
//! ```tool
//! LIST_DIR
//! path: src
//! ```
//! ````

pub mod parser;

pub use self::parser::normalize_function_calls;
pub use self::parser::parse;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    ReadFile {
        path: String,
        offset: Option<usize>,
        limit: Option<usize>,
    },
    WriteFile {
        path: String,
        content: String,
    },
    EditFile {
        path: String,
        search: String,
        replace: String,
    },
    ListDir {
        path: String,
    },
    RunCommand {
        command: String,
        cwd: Option<String>,
        timeout: Option<u32>,
    },
}

impl Action {
    pub fn verb(&self) -> &'static str {
        match self {
            Action::ReadFile { .. } => "READ_FILE",
            Action::WriteFile { .. } => "WRITE_FILE",
            Action::EditFile { .. } => "EDIT_FILE",
            Action::ListDir { .. } => "LIST_DIR",
            Action::RunCommand { .. } => "RUN_COMMAND",
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Action::ReadFile { path, .. }
            | Action::WriteFile { path, .. }
            | Action::EditFile { path, .. }
            | Action::ListDir { path } => path,
            Action::RunCommand { command, .. } => command,
        }
    }
}

/// Result of parsing one model reply.
#[derive(Debug, Default)]
pub struct ParsedOutput {
    /// Everything that is not a tool block (the model's prose).
    pub prose: String,
    /// Valid actions, in emission order.
    pub actions: Vec<Action>,
    /// Blocks that looked like tool blocks but failed validation
    /// (surfaced to the user so the model's intent isn't silently lost).
    pub invalid_blocks: Vec<String>,
}

/// Internal marker lines that only ever appear in *our* tool-result
/// messages. When a model echoes them back they are noise, never content.
pub fn is_internal_marker(line: &str) -> bool {
    let t = line.trim();
    t == "[TOOL RESULTS]" || t == "--- end ---"
}

/// Line-level output hygiene for model text: drops echoed internal
/// markers and collapses runaway repetition (small local models sometimes
/// degenerate into repeating one line forever).
///
/// `min_repeat_len` guards legitimate short repeats (e.g. closing braces in
/// code): only lines at least this long are collapsed.
pub struct LineSanitizer {
    last: String,
    repeats: usize,
    min_repeat_len: usize,
}

impl LineSanitizer {
    pub fn new(min_repeat_len: usize) -> Self {
        LineSanitizer {
            last: String::new(),
            repeats: 0,
            min_repeat_len,
        }
    }

    /// Feed one line; `None` means the line should be dropped.
    pub fn push(&mut self, line: &str) -> Option<String> {
        if is_internal_marker(line) {
            return None;
        }
        let empty = line.trim().is_empty();
        if !empty && line.trim_end() == self.last {
            self.repeats += 1;
            if self.repeats >= 3 && line.trim().chars().count() >= self.min_repeat_len {
                return None;
            }
        } else {
            self.last = line.trim_end().to_string();
            self.repeats = 1;
        }
        Some(line.to_string())
    }
}

/// Whole-message variant of [`LineSanitizer`] for history storage.
pub fn sanitize_text(text: &str, min_repeat_len: usize) -> String {
    let mut san = LineSanitizer::new(min_repeat_len);
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(l) = san.push(line) {
            out.push(l);
        }
    }
    out.join("\n")
}

/// Short human-readable label, e.g. `READ_FILE src/main.rs`.
pub fn action_label(action: &Action) -> String {
    match action {
        Action::ReadFile {
            path,
            offset,
            limit,
        } => {
            let mut s = format!("READ_FILE {path}");
            if let Some(o) = offset {
                s.push_str(&format!(" (from line {o}"));
                if let Some(l) = limit {
                    s.push_str(&format!(", {l} lines"));
                }
                s.push(')');
            }
            s
        }
        Action::WriteFile { path, content } => {
            format!("WRITE_FILE {path} ({} lines)", content.lines().count())
        }
        Action::EditFile { path, search, .. } => {
            format!(
                "EDIT_FILE {path} ({} line(s) to replace)",
                search.lines().count().max(1)
            )
        }
        Action::ListDir { path } => format!("LIST_DIR {path}"),
        Action::RunCommand { command, .. } => {
            let trimmed = command.trim();
            if trimmed.chars().count() > 60 {
                let short: String = trimmed.chars().take(60).collect();
                format!("RUN_COMMAND {short}…")
            } else {
                format!("RUN_COMMAND {trimmed}")
            }
        }
    }
}

/// Text shown to the model explaining the protocol. Kept compact on purpose:
/// context budget is precious on 4K models.
pub fn tool_protocol_doc() -> &'static str {
    r#"TOOLS — to use a tool, output a block exactly like these examples:

```tool
READ_FILE
path: src/main.rs
```
Read a file before editing it. Optional keys: `offset: <first line>`, `limit: <max lines>`.

```tool
WRITE_FILE
path: docs/notes.md
content:
<file content, any text>
END_OF_CONTENT
```
Create a file or replace it entirely. For a small change, use EDIT_FILE instead.

```tool
EDIT_FILE
path: src/main.rs
<<<<<<< SEARCH
<exact text to find — must match one unique place>
=======
<replacement text>
>>>>>>> REPLACE
```
Change a small part of an existing file. SEARCH must match the file exactly (whitespace included) in exactly one place.

```tool
LIST_DIR
path: src
```
Explore a directory to see what exists.

```tool
RUN_COMMAND
command: cargo build --release
```
Run a shell command (build, tests, scripts). The user is asked to confirm every command. Optional keys: `cwd: <subdir>`, `timeout: <seconds, max 300>`. Never use it for file changes - use the file tools.

HOW TO WORK
1. Understand what exists first: use LIST_DIR and READ_FILE before changing anything.
2. One fenced block holds exactly one action; you may emit several blocks per reply.
3. After each action you get a [TOOL RESULTS] message. Read it, then continue with the next step.
4. Never repeat an action that already succeeded. If a result says the file is unchanged or already correct, move on to the next step.
5. If a tool returns an error, fix the cause (for example a wrong path) instead of retrying the same call.
6. When the task is finished, reply to the user in plain text, briefly, with no tool block.

RULES
- Tool calls are ONLY the fenced tool blocks shown above. Never use JSON, XML tags (like `<function>`), or any other format.
- Never invent tool names or keys: READ_FILE, WRITE_FILE, EDIT_FILE, LIST_DIR, RUN_COMMAND.
- Do not only describe planned changes — make them with the tools.
- Paths are relative to the project root.
- Never write a line containing only END_OF_CONTENT inside file content.

EXAMPLE OF ONE CHANGE
User: "fix the greeting in hello.txt"
You:
```tool
READ_FILE
path: hello.txt
```
[TOOL RESULTS] show the file content: "Helo world!"
You:
```tool
EDIT_FILE
path: hello.txt
<<<<<<< SEARCH
Helo world!
=======
Hello world!
>>>>>>> REPLACE
```
[TOOL RESULTS] say the file was edited.
You:
Fixed the greeting in hello.txt."#
}

#[cfg(test)]
mod doc_tests {
    use super::*;

    /// The worked examples inside the prompt must stay valid per the
    /// parser - a drifting example teaches weak models the wrong format.
    #[test]
    fn tool_doc_examples_parse() {
        let p = parse(tool_protocol_doc());
        assert!(
            p.invalid_blocks.is_empty(),
            "invalid: {:?}",
            p.invalid_blocks
        );
        assert_eq!(
            p.actions.len(),
            7,
            "expected 5 syntax examples + 2 example-of-change blocks: {:?}",
            p.actions
        );
        assert!(matches!(p.actions[0], Action::ReadFile { .. }));
        assert!(matches!(p.actions[1], Action::WriteFile { .. }));
        assert!(matches!(p.actions[2], Action::EditFile { .. }));
        assert!(matches!(p.actions[3], Action::ListDir { .. }));
        assert!(matches!(p.actions[4], Action::RunCommand { .. }));
        assert!(matches!(p.actions[5], Action::ReadFile { .. }));
        assert!(matches!(p.actions[6], Action::EditFile { .. }));
    }
}
