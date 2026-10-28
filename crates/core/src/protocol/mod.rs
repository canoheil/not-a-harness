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
}

impl Action {
    pub fn verb(&self) -> &'static str {
        match self {
            Action::ReadFile { .. } => "READ_FILE",
            Action::WriteFile { .. } => "WRITE_FILE",
            Action::EditFile { .. } => "EDIT_FILE",
            Action::ListDir { .. } => "LIST_DIR",
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Action::ReadFile { path, .. }
            | Action::WriteFile { path, .. }
            | Action::EditFile { path, .. }
            | Action::ListDir { path } => path,
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
    }
}

/// Text shown to the model explaining the protocol. Kept compact on purpose:
/// context budget is precious on 4K models.
pub fn tool_protocol_doc() -> &'static str {
    "TOOLS — to use a tool, output a block exactly like these examples:\n\
\n\
```tool\n\
READ_FILE\n\
path: src/main.rs\n\
```\n\
(optional keys: `offset: <first line>`, `limit: <max lines>`)\n\
\n\
```tool\n\
WRITE_FILE\n\
path: docs/notes.md\n\
content:\n\
<file content, any text>\n\
END_OF_CONTENT\n\
```\n\
\n\
```tool\n\
EDIT_FILE\n\
path: src/main.rs\n\
<<<<<<< SEARCH\n\
<exact text to find — must match one unique place>\n\
=======\n\
<replacement text>\n\
>>>>>>> REPLACE\n\
```\n\
\n\
```tool\n\
LIST_DIR\n\
path: src\n\
```\n\
\n\
Rules:\n\
- Each fenced block holds exactly one action; you may emit several blocks per reply.\n\
- SEARCH text must match the file exactly (whitespace included) in exactly one place.\n\
- Read a file before editing it. Paths are relative to the project root.\n\
- Never write a line containing only END_OF_CONTENT inside file content.\n\
- When the task is finished, reply in plain text without any tool block."
}
