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

HOW TO WORK
1. Understand what exists first: use LIST_DIR and READ_FILE before changing anything.
2. One fenced block holds exactly one action; you may emit several blocks per reply.
3. After each action you get a [TOOL RESULTS] message. Read it, then continue with the next step.
4. Never repeat an action that already succeeded. If a result says the file is unchanged or already correct, move on to the next step.
5. If a tool returns an error, fix the cause (for example a wrong path) instead of retrying the same call.
6. When the task is finished, reply to the user in plain text, briefly, with no tool block.

RULES
- Never use JSON. Only the fenced tool blocks shown above.
- Never invent tool names or keys: READ_FILE, WRITE_FILE, EDIT_FILE, LIST_DIR.
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
            6,
            "expected 4 syntax examples + 2 example-of-change blocks: {:?}",
            p.actions
        );
        assert!(matches!(p.actions[0], Action::ReadFile { .. }));
        assert!(matches!(p.actions[1], Action::WriteFile { .. }));
        assert!(matches!(p.actions[2], Action::EditFile { .. }));
        assert!(matches!(p.actions[3], Action::ListDir { .. }));
        assert!(matches!(p.actions[4], Action::ReadFile { .. }));
        assert!(matches!(p.actions[5], Action::EditFile { .. }));
    }
}
