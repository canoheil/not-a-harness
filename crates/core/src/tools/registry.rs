//! Central registry of the tools the harness offers.
//!
//! One place answers three questions: which tools exist (`ToolDef`),
//! which are currently enabled, and which prompt section each tool
//! contributes. `visible()` is the seam where a future global Mode
//! (read-only / confirm / auto) will also filter tools dynamically -
//! v1 has no modes and returns every enabled tool.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Builtin,
    /// User-defined tools from `~/.config/nah/tools/` (loaded later).
    Custom,
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub kind: ToolKind,
    /// Prompt section (example fence + one-line description) shown to the
    /// model whenever the tool is enabled.
    pub doc: String,
}

#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<ToolDef>,
    disabled: Vec<String>,
}

const DOC_READ: &str = r#"```tool
READ_FILE
path: src/main.rs
```
Read a file before editing it. Optional keys: `offset: <first line>`, `limit: <max lines>`."#;

const DOC_WRITE: &str = r#"```tool
WRITE_FILE
path: docs/notes.md
content:
<file content, any text>
END_OF_CONTENT
```
Create a file or replace it entirely. For a small change, use EDIT_FILE instead."#;

const DOC_EDIT: &str = r#"```tool
EDIT_FILE
path: src/main.rs
<<<<<<< SEARCH
<exact text to find — must match one unique place>
=======
<replacement text>
>>>>>>> REPLACE
```
Change a small part of an existing file. SEARCH must match the file exactly (whitespace included) in exactly one place."#;

const DOC_LIST: &str = r#"```tool
LIST_DIR
path: src
```
Explore a directory to see what exists."#;

const DOC_RUN: &str = r#"```tool
RUN_COMMAND
command: cargo build --release
```
Run a shell command (build, tests, scripts). The user is asked to confirm every command. Optional keys: `cwd: <subdir>`, `timeout: <seconds, max 300>`. Never use it for file changes - use the file tools."#;

const DOC_DELETE: &str = r#"```tool
DELETE_FILE
path: old.txt
```
Delete a file. The user is asked to confirm before anything is removed."#;

const DOC_MOVE: &str = r#"```tool
MOVE_FILE
path: old.txt
to: new.txt
```
Move or rename a file. The user is asked to confirm before anything changes."#;

impl ToolRegistry {
    /// All built-in tools, all enabled.
    pub fn builtin() -> Self {
        let tools = [
            ("READ_FILE", DOC_READ),
            ("WRITE_FILE", DOC_WRITE),
            ("EDIT_FILE", DOC_EDIT),
            ("LIST_DIR", DOC_LIST),
            ("RUN_COMMAND", DOC_RUN),
            ("DELETE_FILE", DOC_DELETE),
            ("MOVE_FILE", DOC_MOVE),
        ]
        .into_iter()
        .map(|(name, doc)| ToolDef {
            name: name.to_string(),
            kind: ToolKind::Builtin,
            doc: doc.to_string(),
        })
        .collect();
        Self {
            tools,
            disabled: Vec::new(),
        }
    }

    /// Tools the model may use, in prompt order. The future global Mode
    /// (read-only / confirm / auto) will filter in this same place.
    pub fn visible(&self) -> impl Iterator<Item = &ToolDef> {
        self.tools.iter().filter(|t| self.enabled(&t.name))
    }

    pub fn all(&self) -> &[ToolDef] {
        &self.tools
    }

    pub fn enabled(&self, name: &str) -> bool {
        !self.disabled.iter().any(|d| d.eq_ignore_ascii_case(name))
    }

    /// Enable or disable a known tool. Unknown names return `false`.
    pub fn set_enabled(&mut self, name: &str, on: bool) -> bool {
        let Some(def) = self
            .tools
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
        else {
            return false;
        };
        let name = def.name.clone();
        self.disabled.retain(|d| !d.eq_ignore_ascii_case(&name));
        if !on {
            self.disabled.push(name);
        }
        true
    }

    /// Disabled tool names (persisted in the config file).
    pub fn disabled_names(&self) -> &[String] {
        &self.disabled
    }

    /// Apply a persisted disabled list.
    pub fn set_disabled_list(&mut self, names: Vec<String>) {
        self.disabled = names;
    }

    /// Comma-separated enabled names for teaching error messages.
    pub fn enabled_names(&self) -> String {
        self.visible()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_offers_seven_tools_all_enabled() {
        let reg = ToolRegistry::builtin();
        assert_eq!(reg.all().len(), 7);
        assert_eq!(reg.visible().count(), 7);
    }

    #[test]
    fn disable_hides_tool_from_visible() {
        let mut reg = ToolRegistry::builtin();
        assert!(reg.set_enabled("LIST_DIR", false));
        assert!(!reg.enabled("list_dir")); // case-insensitive
        assert_eq!(reg.visible().count(), 6);
        assert!(!reg.visible().any(|t| t.name == "LIST_DIR"));
        // unknown names refuse to toggle
        assert!(!reg.set_enabled("NOT_A_TOOL", false));
        // re-enable works and clears the persisted list
        assert!(reg.set_enabled("LIST_DIR", true));
        assert!(reg.disabled_names().is_empty());
    }

    #[test]
    fn disabled_list_roundtrip() {
        let mut reg = ToolRegistry::builtin();
        reg.set_disabled_list(vec!["RUN_COMMAND".to_string()]);
        assert!(!reg.enabled("RUN_COMMAND"));
        assert!(reg.enabled("READ_FILE"));
        assert!(reg.enabled_names().contains("READ_FILE"));
        assert!(!reg.enabled_names().contains("RUN_COMMAND"));
    }
}
