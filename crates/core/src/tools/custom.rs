//! User-defined tools loaded from the global tools directory
//! (`~/.config/nah/tools/*.tool`).
//!
//! The format is deliberately linear `key: value` (no JSON/TOML) so the
//! parser stays small and the files stay hand-writable:
//!
//! ```text
//! name: NMAP_SCAN
//! description: Scan a host with nmap; returns open ports and services.
//! params:
//!   target: host or IP to scan
//!   ports: port range like 1-1000
//! command: nmap -sV -p {ports} {target}
//! timeout: 120
//! ```
//!
//! Optional `command.windows:` / `command.unix:` override `command:` per
//! platform. Every declared parameter is required at call time; unknown
//! top-level keys are reported and skipped. A file that fails validation
//! is skipped with a warning - loading never fails hard.
//!
//! Project-local tool files are intentionally NOT supported (supply
//! chain safety): only the user's own global directory is scanned.

use std::path::Path;

use super::registry::{CustomSpec, ToolDef, ToolKind};

/// Maximum tool files scanned from one directory.
const MAX_FILES: usize = 64;

/// Valid tool name: `A-Z`, `0-9`, `_`, starting with a letter.
pub fn is_tool_name(s: &str) -> bool {
    s.len() >= 2
        && s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Valid parameter name: letters, digits, `_`, starting with a letter.
fn is_param_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The prompt section for a custom tool: example fence + description.
pub fn custom_doc(name: &str, description: &str, params: &[(String, String)]) -> String {
    let mut fence = format!("```tool\n{name}\n");
    for (pname, help) in params {
        fence.push_str(&format!("{pname}: <{help}>\n"));
    }
    fence.push_str("```\n");
    fence.push_str(description);
    fence.push_str(" Every parameter is required.");
    fence
}

/// Parse one `.tool` file. Returns the definition plus non-fatal
/// warnings, or a skip reason.
pub fn parse_tool_file(text: &str, source: &str) -> Result<(ToolDef, Vec<String>), String> {
    let mut name = String::new();
    let mut description = String::new();
    let mut spec = CustomSpec {
        timeout: 30,
        ..Default::default()
    };
    let mut warnings = Vec::new();
    let mut in_params = false;

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if !in_params {
                return Err(format!("{source}: indented line outside a params block"));
            }
            let t = line.trim();
            let Some((pname, help)) = t.split_once(':') else {
                return Err(format!("{source}: params entry '{t}' is not `name: help`"));
            };
            let pname = pname.trim();
            if !is_param_name(pname) {
                return Err(format!("{source}: bad parameter name '{pname}'"));
            }
            if spec.params.iter().any(|(p, _)| p == pname) {
                return Err(format!("{source}: duplicate parameter '{pname}'"));
            }
            spec.params
                .push((pname.to_string(), help.trim().to_string()));
            continue;
        }
        in_params = false;
        let Some((key, value)) = line.split_once(':') else {
            return Err(format!("{source}: line '{line}' is not `key: value`"));
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "name" => name = value.to_string(),
            "description" => description = value.to_string(),
            "params" => in_params = true,
            "command" => spec.command = value.to_string(),
            "command.windows" => spec.command_windows = Some(value.to_string()),
            "command.unix" => spec.command_unix = Some(value.to_string()),
            "timeout" => {
                spec.timeout = value
                    .parse::<u32>()
                    .map_err(|_| format!("{source}: bad timeout '{value}'"))?
                    .clamp(1, 300);
            }
            other => warnings.push(format!("{source}: unknown key '{other}' ignored")),
        }
    }

    if !is_tool_name(&name) {
        return Err(format!(
            "{source}: bad name '{name}' (use uppercase letters, digits, _)"
        ));
    }
    if description.is_empty() {
        return Err(format!("{source}: description is required"));
    }
    if spec.command.is_empty() && spec.command_windows.is_none() && spec.command_unix.is_none() {
        return Err(format!("{source}: command is required"));
    }
    // every {placeholder} in a template must be a declared parameter
    for template in [
        &spec.command,
        &spec.command_windows.clone().unwrap_or_default(),
        &spec.command_unix.clone().unwrap_or_default(),
    ] {
        let mut rest = template.as_str();
        while let Some(open) = rest.find('{') {
            let Some(close) = rest[open..].find('}') else {
                return Err(format!("{source}: unmatched '{{' in command"));
            };
            let placeholder = &rest[open + 1..open + close];
            if !spec.params.iter().any(|(p, _)| p == placeholder) {
                return Err(format!(
                    "{source}: command uses '{{{placeholder}}}' but the parameter is not declared"
                ));
            }
            rest = &rest[open + close + 1..];
        }
    }

    let doc = custom_doc(&name, &description, &spec.params);
    Ok((
        ToolDef {
            name,
            kind: ToolKind::Custom,
            doc,
            custom: Some(spec),
        },
        warnings,
    ))
}

/// Scan `dir` for `*.tool` files. Returns the parsed definitions and a
/// warning per skipped file. Missing directory is not an error.
pub fn load_dir(dir: &Path) -> (Vec<ToolDef>, Vec<String>) {
    let mut defs = Vec::new();
    let mut warnings = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (defs, warnings);
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "tool"))
        .collect();
    files.sort();
    for path in files.into_iter().take(MAX_FILES) {
        let source = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_tool_file(&text, &source) {
                Ok((def, notes)) => {
                    warnings.extend(notes);
                    defs.push(def);
                }
                Err(reason) => warnings.push(format!("skipped {reason}")),
            },
            Err(e) => warnings.push(format!("skipped '{source}': {e}")),
        }
    }
    (defs, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
name: NMAP_SCAN
description: Scan a host with nmap; returns open ports.
params:
  target: host or IP to scan
  ports: port range like 1-1000
command: nmap -sV -p {ports} {target}
timeout: 120
";

    #[test]
    fn parses_a_full_tool_file() {
        let (def, notes) = parse_tool_file(SAMPLE, "nmap.tool").unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(def.name, "NMAP_SCAN");
        assert_eq!(def.kind, ToolKind::Custom);
        let spec = def.custom.unwrap();
        assert_eq!(spec.params.len(), 2);
        assert_eq!(spec.params[0].0, "target");
        assert_eq!(spec.timeout, 120);
        assert_eq!(
            spec.substitute(&[
                ("target".to_string(), "10.0.0.5".to_string()),
                ("ports".to_string(), "1-100".to_string())
            ]),
            "nmap -sV -p 1-100 10.0.0.5"
        );
        // the prompt section shows the parameters
        assert!(def.doc.contains("target: <host or IP to scan>"));
        assert!(def.doc.contains("Every parameter is required."));
    }

    #[test]
    fn bad_files_are_rejected_with_reasons() {
        // bad name
        let err = parse_tool_file("name: lower\ndescription: x\ncommand: echo hi\n", "a.tool")
            .unwrap_err();
        assert!(err.contains("bad name"), "{err}");
        // missing description
        let err = parse_tool_file("name: OK_NAME\ncommand: echo hi\n", "b.tool").unwrap_err();
        assert!(err.contains("description"), "{err}");
        // missing command
        let err = parse_tool_file("name: OK_NAME\ndescription: x\n", "c.tool").unwrap_err();
        assert!(err.contains("command"), "{err}");
        // undeclared placeholder
        let err = parse_tool_file(
            "name: OK_NAME\ndescription: x\ncommand: echo {who}\n",
            "d.tool",
        )
        .unwrap_err();
        assert!(err.contains("not declared"), "{err}");
    }

    #[test]
    fn platform_overrides_and_unknown_keys() {
        let (def, notes) = parse_tool_file(
            "name: LISTY\ndescription: list things\ncommand: ls\n\
             command.windows: dir\ncommand.unix: ls -la\nmystery: 1\n",
            "e.tool",
        )
        .unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        let spec = def.custom.unwrap();
        assert_eq!(spec.command_windows.as_deref(), Some("dir"));
        assert_eq!(spec.command_unix.as_deref(), Some("ls -la"));
        // platform template wins over the generic one
        assert_eq!(
            spec.template_for_platform(),
            if cfg!(windows) { "dir" } else { "ls -la" }
        );
    }

    #[test]
    fn tool_and_param_names_are_validated() {
        assert!(is_tool_name("NMAP_SCAN"));
        assert!(!is_tool_name("nmap"));
        assert!(!is_tool_name("1BAD"));
        assert!(!is_tool_name("BAD-NAME"));
        assert!(!is_tool_name("A"));
    }
}
