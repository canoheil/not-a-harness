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
    DeleteFile {
        path: String,
    },
    MoveFile {
        path: String,
        to: String,
    },
    /// A user-defined tool call: free-form name + `key: value` params.
    Custom {
        name: String,
        params: Vec<(String, String)>,
    },
}

impl Action {
    pub fn verb(&self) -> String {
        match self {
            Action::ReadFile { .. } => "READ_FILE".to_string(),
            Action::WriteFile { .. } => "WRITE_FILE".to_string(),
            Action::EditFile { .. } => "EDIT_FILE".to_string(),
            Action::ListDir { .. } => "LIST_DIR".to_string(),
            Action::RunCommand { .. } => "RUN_COMMAND".to_string(),
            Action::DeleteFile { .. } => "DELETE_FILE".to_string(),
            Action::MoveFile { .. } => "MOVE_FILE".to_string(),
            Action::Custom { name, .. } => name.clone(),
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Action::ReadFile { path, .. }
            | Action::WriteFile { path, .. }
            | Action::EditFile { path, .. }
            | Action::ListDir { path }
            | Action::DeleteFile { path }
            | Action::MoveFile { path, .. } => path,
            Action::RunCommand { command, .. } => command,
            Action::Custom { .. } => "",
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
/// messages. When a model echoes them back (often with made-up
/// continuations like `[TOOL RESULTS] say the file was edited.`) they are
/// noise, never content. Template think tags count too.
pub fn is_internal_marker(line: &str) -> bool {
    let t = line.trim();
    let lower = t.to_ascii_lowercase();
    lower.starts_with("[tool result") // [TOOL RESULTS] / [tool result] - any continuation
        || lower.starts_with("--- end ---")
        || lower.starts_with("--- output ---")
        || lower.starts_with("--- end of output")
        || matches!(t, "<think>" | "</think>" | "</output>" | "<output>")
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
        Action::DeleteFile { path } => format!("DELETE_FILE {path}"),
        Action::MoveFile { path, to } => format!("MOVE_FILE {path} -> {to}"),
        Action::Custom { name, params } => {
            let args: Vec<String> = params.iter().map(|(k, v)| format!("{k}: {v}")).collect();
            format!("{name} {}", args.join(", "))
        }
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
/// Prompt document: fixed preamble + a section per visible tool +
/// rules + worked example. Only enabled tools are documented - a lean
/// tool list keeps small models out of trouble.
pub fn tool_protocol_doc(reg: &crate::tools::ToolRegistry) -> String {
    let mut out =
        String::from("TOOLS — to use a tool, output a block exactly like these examples:\n\n");
    for def in reg.visible() {
        out.push_str(&def.doc);
        out.push_str("\n\n");
    }
    out.push_str(HOW_TO_WORK);
    out.push_str(RULES_HEAD);
    out.push_str(&format!(
        "- Never invent tool names or keys: {}.\n",
        reg.enabled_names()
    ));
    out.push_str(RULES_TAIL);
    out.push_str(EXAMPLE);
    out
}

const HOW_TO_WORK: &str = "HOW TO WORK\n1. Understand what exists first: explore the project and read files before changing anything.\n2. One fenced block holds exactly one action; you may emit several blocks per reply.\n3. After each action you get a [TOOL RESULTS] message. Read it, then continue with the next step.\n4. Never repeat an action that already succeeded. If a result says the file is unchanged or already correct, move on to the next step.\n5. If a tool returns an error, fix the cause (for example a wrong path) instead of retrying the same call.\n6. When the task is finished, reply to the user in plain text, briefly, with no tool block.\n\n";

const RULES_HEAD: &str = "RULES\n- Tool calls are ONLY the fenced tool blocks shown above. Never use JSON, XML tags (like `<function>`), or any other format.\n- Never write `[TOOL RESULTS]` or fake tool results - the system sends those messages, you never do.\n- Never claim a step succeeded unless a tool result confirms it; report failures honestly.\n";

const RULES_TAIL: &str = "- Do not only describe planned changes — make them with the tools.\n- Paths are relative to the project root.\n- Never write a line containing only END_OF_CONTENT inside file content.\n\n";

const EXAMPLE: &str = r#"EXAMPLE OF ONE CHANGE
User: "fix the greeting in hello.txt"
Your reply contains a tool block:
```tool
READ_FILE
path: hello.txt
```
The system then sends you a tool result message (never write these yourself):
[TOOL RESULTS]
Helo world!
--- end ---
Your next reply contains:
```tool
EDIT_FILE
path: hello.txt
<<<<<<< SEARCH
Helo world!
=======
Hello world!
>>>>>>> REPLACE
```
The system replies:
[TOOL RESULTS]
edited 'hello.txt' in one place.
--- end ---
Your final reply is plain text:
Fixed the greeting in hello.txt."#;

#[cfg(test)]
mod doc_tests {
    use super::*;

    #[test]
    fn marker_detection_covers_fake_forms() {
        // real markers
        assert!(is_internal_marker("[TOOL RESULTS]"));
        assert!(is_internal_marker("--- end ---"));
        // fake narration with continuation (observed from real models)
        assert!(is_internal_marker("[TOOL RESULTS] show the file list."));
        assert!(is_internal_marker("[tool result] READ_FILE test.txt (ok)"));
        assert!(is_internal_marker("--- output ---"));
        assert!(is_internal_marker("--- end of output"));
        // template think tags
        assert!(is_internal_marker("</think>"));
        assert!(is_internal_marker("<think>"));
        // ordinary prose is untouched
        assert!(!is_internal_marker("The file is ready."));
    }

    /// The worked examples inside the prompt must stay valid per the
    /// parser - a drifting example teaches weak models the wrong format.
    #[test]
    fn tool_doc_examples_parse() {
        let p = parse(&tool_protocol_doc(&crate::tools::ToolRegistry::builtin()));
        assert!(
            p.invalid_blocks.is_empty(),
            "invalid: {:?}",
            p.invalid_blocks
        );
        assert_eq!(
            p.actions.len(),
            9,
            "expected 7 syntax examples + 2 example-of-change blocks: {:?}",
            p.actions
        );
        assert!(matches!(p.actions[0], Action::ReadFile { .. }));
        assert!(matches!(p.actions[1], Action::WriteFile { .. }));
        assert!(matches!(p.actions[2], Action::EditFile { .. }));
        assert!(matches!(p.actions[3], Action::ListDir { .. }));
        assert!(matches!(p.actions[4], Action::RunCommand { .. }));
        assert!(matches!(p.actions[5], Action::DeleteFile { .. }));
        assert!(matches!(p.actions[6], Action::MoveFile { .. }));
        assert!(matches!(p.actions[7], Action::ReadFile { .. }));
        assert!(matches!(p.actions[8], Action::EditFile { .. }));
    }

    /// Only enabled tools reach the prompt - disabling one must drop
    /// both its section and its name from the rules line.
    #[test]
    fn tool_doc_only_lists_enabled_tools() {
        let mut reg = crate::tools::ToolRegistry::builtin();
        reg.set_enabled("LIST_DIR", false);
        reg.set_enabled("MOVE_FILE", false);
        let doc = tool_protocol_doc(&reg);
        assert!(!doc.contains("LIST_DIR"), "disabled tool leaked:\n{doc}");
        assert!(!doc.contains("MOVE_FILE"), "disabled tool leaked:\n{doc}");
        assert!(doc.contains("READ_FILE"));
        assert!(doc.contains("DELETE_FILE"));
        // the teaching line lists exactly the enabled names
        assert!(!doc.contains(
            "Never invent tool names or keys: READ_FILE, WRITE_FILE, EDIT_FILE, LIST_DIR"
        ));
    }
}
