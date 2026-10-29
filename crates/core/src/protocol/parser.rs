//! Line-oriented state machine for the text tool protocol.
//!
//! Design notes:
//! * No regex: exact line matching is more predictable and regex-free code
//!   keeps the binary tiny.
//! * A block is only turned into an `Action` when it is *complete and valid*.
//!   Half-finished or bogus blocks fall back to prose so we never execute
//!   something the model merely mentioned in passing.

use super::{Action, ParsedOutput};

const OPEN: &str = "```tool";
const CLOSE: &str = "```";
const CONTENT_END: &str = "END_OF_CONTENT";
const SEARCH_MARK: &str = "<<<<<<< SEARCH";
const MID_MARK: &str = "=======";
const REPLACE_MARK: &str = ">>>>>>> REPLACE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Prose,
    ToolName,
    Params,
    Content,
    Search,
    Replace,
}

pub fn parse(text: &str) -> ParsedOutput {
    // small non-coder models often emit `<function name="...">` XML calls
    // instead of fenced blocks - fold those into the fenced form first.
    let text = normalize_function_calls(text);
    let mut out = ParsedOutput::default();

    // prose lines accumulated so far
    let mut prose: Vec<String> = Vec::new();
    // lines of the tool block currently being read (kept for fallback)
    let mut block: Vec<String> = Vec::new();
    let mut state = State::Prose;

    let mut name = String::new();
    let mut path: Option<String> = None;
    let mut offset: Option<usize> = None;
    let mut limit: Option<usize> = None;
    let mut content: Vec<String> = Vec::new();
    let mut search: Vec<String> = Vec::new();
    let mut replace: Vec<String> = Vec::new();
    let mut command: Option<String> = None;
    let mut cmd_cwd: Option<String> = None;
    let mut cmd_timeout: Option<u32> = None;

    // drop the candidate block back into prose (aborted / malformed)
    fn abort(block: &mut Vec<String>, prose: &mut Vec<String>) {
        prose.append(block);
    }

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');

        match state {
            State::Prose => {
                if line.trim_end() == OPEN {
                    block.clear();
                    block.push(line.to_string());
                    state = State::ToolName;
                } else {
                    prose.push(line.to_string());
                }
            }
            State::ToolName => {
                block.push(line.to_string());
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                if matches!(
                    t,
                    "READ_FILE" | "WRITE_FILE" | "EDIT_FILE" | "LIST_DIR" | "RUN_COMMAND"
                ) {
                    name = t.to_string();
                    state = State::Params;
                } else {
                    abort(&mut block, &mut prose);
                    state = State::Prose;
                }
            }
            State::Params => {
                block.push(line.to_string());
                let t = line.trim();
                if t == CLOSE {
                    match make_action(
                        &name,
                        &path,
                        offset,
                        &limit,
                        &content,
                        &search,
                        &replace,
                        &command,
                        &cmd_cwd,
                        cmd_timeout,
                    ) {
                        Some(action) => {
                            block.clear(); // consumed
                            out.actions.push(action);
                        }
                        None => {
                            out.invalid_blocks.push(block.join("\n"));
                            block.clear();
                        }
                    }
                    reset(
                        &mut name,
                        &mut path,
                        &mut offset,
                        &mut limit,
                        &mut content,
                        &mut search,
                        &mut replace,
                        &mut command,
                        &mut cmd_cwd,
                        &mut cmd_timeout,
                    );
                    state = State::Prose;
                } else if t == "content:" && name == "WRITE_FILE" {
                    state = State::Content;
                } else if t == SEARCH_MARK && name == "EDIT_FILE" {
                    state = State::Search;
                } else if let Some((key, value)) = t.split_once(':') {
                    let key = key.trim();
                    let value = value.trim();
                    match key {
                        "path" => {
                            if !value.is_empty() {
                                path = Some(value.to_string());
                            }
                        }
                        "offset" => offset = value.parse().ok(),
                        "limit" => limit = value.parse().ok(),
                        "command" => {
                            if !value.is_empty() {
                                command = Some(value.to_string());
                            }
                        }
                        "cwd" => {
                            if !value.is_empty() {
                                cmd_cwd = Some(value.to_string());
                            }
                        }
                        "timeout" => cmd_timeout = value.parse().ok(),
                        _ => {} // ignore unknown keys, stay tolerant
                    }
                } else if t.is_empty() {
                    // blank lines between params are fine
                } else if matches!(t, MID_MARK | REPLACE_MARK | CONTENT_END) {
                    // degenerate duplicate closing markers (small models
                    // sometimes echo them after a complete pair) - ignore
                } else {
                    abort(&mut block, &mut prose);
                    reset(
                        &mut name,
                        &mut path,
                        &mut offset,
                        &mut limit,
                        &mut content,
                        &mut search,
                        &mut replace,
                        &mut command,
                        &mut cmd_cwd,
                        &mut cmd_timeout,
                    );
                    state = State::Prose;
                }
            }
            State::Content => {
                block.push(line.to_string());
                if line == CONTENT_END {
                    state = State::Params; // expect closing fence next
                } else {
                    content.push(line.to_string());
                }
            }
            State::Search => {
                block.push(line.to_string());
                if line == MID_MARK {
                    state = State::Replace;
                } else {
                    search.push(line.to_string());
                }
            }
            State::Replace => {
                block.push(line.to_string());
                if line == REPLACE_MARK {
                    state = State::Params;
                } else {
                    replace.push(line.to_string());
                }
            }
        }
    }

    // unterminated block at EOF → everything stays prose
    if !block.is_empty() {
        abort(&mut block, &mut prose);
    }
    out.prose = prose.join("\n");
    out
}

#[allow(clippy::too_many_arguments)]
fn make_action(
    name: &str,
    path: &Option<String>,
    offset: Option<usize>,
    limit: &Option<usize>,
    content: &[String],
    search: &[String],
    replace: &[String],
    command: &Option<String>,
    cmd_cwd: &Option<String>,
    cmd_timeout: Option<u32>,
) -> Option<Action> {
    if name == "RUN_COMMAND" {
        let command = command.clone()?;
        return Some(Action::RunCommand {
            command,
            cwd: cmd_cwd.clone(),
            timeout: cmd_timeout,
        });
    }
    let p = path.clone()?;
    match name {
        "READ_FILE" => Some(Action::ReadFile {
            path: p,
            offset,
            limit: *limit,
        }),
        "WRITE_FILE" => {
            let mut text = content.join("\n");
            if !text.is_empty() {
                text.push('\n');
            }
            Some(Action::WriteFile {
                path: p,
                content: text,
            })
        }
        "EDIT_FILE" => {
            if search.is_empty() && replace.is_empty() {
                return None;
            }
            Some(Action::EditFile {
                path: p,
                search: search.join("\n"),
                replace: replace.join("\n"),
            })
        }
        "LIST_DIR" => Some(Action::ListDir { path: p }),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn reset(
    name: &mut String,
    path: &mut Option<String>,
    offset: &mut Option<usize>,
    limit: &mut Option<usize>,
    content: &mut Vec<String>,
    search: &mut Vec<String>,
    replace: &mut Vec<String>,
    command: &mut Option<String>,
    cmd_cwd: &mut Option<String>,
    cmd_timeout: &mut Option<u32>,
) {
    name.clear();
    *path = None;
    *offset = None;
    *limit = None;
    content.clear();
    search.clear();
    replace.clear();
    *command = None;
    *cmd_cwd = None;
    *cmd_timeout = None;
}

/// Fold `<function name="X"> ... </function>` calls into fenced tool
/// blocks. Some chat models have a strong XML function-call prior; their
/// bodies use our parameter syntax, so one rewrite makes them executable.
/// Best-effort and line-oriented - never panics.
pub fn normalize_function_calls(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut body: Vec<String> = Vec::new();
    let mut name: Option<String> = None;

    fn flush(out: &mut Vec<String>, body: &mut Vec<String>, name: &mut Option<String>) {
        if let Some(n) = name.take() {
            out.push(OPEN.to_string());
            out.push(n);
            out.append(body);
            out.push(CLOSE.to_string());
        }
        body.clear();
    }

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let t = line.trim();
        if t.starts_with("<function") {
            flush(&mut out, &mut body, &mut name);
            let gt = t.find('>').map(|i| i + 1).unwrap_or(t.len());
            name = tag_attr(&t[..gt], "name");
            let rest = clean_tags(&t[gt..]);
            if !rest.trim().is_empty() {
                body.push(rest);
            }
            if t.contains("</function>") {
                flush(&mut out, &mut body, &mut name);
            }
        } else if name.is_some() {
            if let Some(idx) = t.find("</function>") {
                let rest = clean_tags(&t[..idx]);
                if !rest.trim().is_empty() {
                    body.push(rest);
                }
                flush(&mut out, &mut body, &mut name);
            } else {
                body.push(clean_tags(line));
            }
        } else {
            out.push(line.to_string());
        }
    }
    flush(&mut out, &mut body, &mut name);
    out.join("\n")
}

/// Strip XML-ish tags from a fragment. `<param name="k">v</param>` and
/// `<name>k</name>` pairs become `k: v`; other tags are dropped. A `<`
/// that does not start a real tag (e.g. `<<<<<<< SEARCH`) is literal text.
fn clean_tags(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(lt) = rest.find('<') {
        let after = &rest[lt + 1..];
        let tag_like = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!');
        if !tag_like {
            out.push_str(&rest[..=lt]); // keep the literal '<'
            rest = after;
            continue;
        }
        out.push_str(&rest[..lt]);
        let Some(gt) = after.find('>') else {
            // unterminated real tag: drop the trailing garbage
            return out.trim_end().to_string();
        };
        let tag = &after[..gt];
        if tag.starts_with("param") || tag.starts_with("name") {
            if let Some(k) = tag_attr(tag, "name") {
                out.push_str(&k);
                out.push_str(": ");
            }
        }
        rest = &after[gt + 1..];
    }
    out.push_str(rest);
    out.trim_end().to_string()
}

/// Extract `attr="value"` (single quotes too) from a tag fragment.
fn tag_attr(tag: &str, attr: &str) -> Option<String> {
    let i = tag.find(attr)? + attr.len();
    let rest = tag[i..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &rest[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Action;

    #[test]
    fn read_file_block() {
        let p = parse("Let me check.\n```tool\nREAD_FILE\npath: src/main.rs\n```\nDone.");
        assert_eq!(p.actions.len(), 1);
        assert_eq!(
            p.actions[0],
            Action::ReadFile {
                path: "src/main.rs".into(),
                offset: None,
                limit: None
            }
        );
        assert!(p.prose.contains("Let me check."), "{:?}", p.prose);
        assert!(p.prose.contains("Done."), "{:?}", p.prose);
        assert!(!p.prose.contains("```tool"), "{:?}", p.prose);
    }

    #[test]
    fn read_file_with_offset_limit() {
        let p = parse("```tool\nREAD_FILE\npath: a.rs\noffset: 10\nlimit: 5\n```");
        assert_eq!(
            p.actions[0],
            Action::ReadFile {
                path: "a.rs".into(),
                offset: Some(10),
                limit: Some(5)
            }
        );
    }

    #[test]
    fn write_file_multiline_with_inner_fences() {
        let text = "```tool\nWRITE_FILE\npath: docs/a.md\ncontent:\nline1\n```\nnot real fence\nline3\nEND_OF_CONTENT\n```";
        let p = parse(text);
        assert_eq!(p.actions.len(), 1);
        match &p.actions[0] {
            Action::WriteFile { path, content } => {
                assert_eq!(path, "docs/a.md");
                assert_eq!(content, "line1\n```\nnot real fence\nline3\n");
            }
            other => panic!("wrong action: {other:?}"),
        }
    }

    #[test]
    fn edit_file_block() {
        let text = "```tool\nEDIT_FILE\npath: x.rs\n<<<<<<< SEARCH\nlet a = 1;\n=======\nlet a = 2;\n>>>>>>> REPLACE\n```";
        let p = parse(text);
        assert_eq!(p.actions.len(), 1);
        match &p.actions[0] {
            Action::EditFile {
                path,
                search,
                replace,
            } => {
                assert_eq!(path, "x.rs");
                assert_eq!(search, "let a = 1;");
                assert_eq!(replace, "let a = 2;");
            }
            other => panic!("wrong action: {other:?}"),
        }
    }

    #[test]
    fn list_dir_block() {
        let p = parse("```tool\nLIST_DIR\npath: .\n```");
        assert_eq!(p.actions[0], Action::ListDir { path: ".".into() });
    }

    #[test]
    fn multiple_blocks_and_prose() {
        let text = "first\n```tool\nLIST_DIR\npath: src\n```\nmiddle\n```tool\nREAD_FILE\npath: a\n```\nlast";
        let p = parse(text);
        assert_eq!(p.actions.len(), 2);
        assert!(p.prose.contains("first"), "{:?}", p.prose);
        assert!(p.prose.contains("middle"), "{:?}", p.prose);
        assert!(p.prose.contains("last"), "{:?}", p.prose);
    }

    #[test]
    fn mentioning_tool_words_in_prose_is_safe() {
        let text = "You can use READ_FILE: src/main.rs like this, and WRITE_FILE: too.\nAlso ```tool\nnot-a-verb\n``` stays prose.";
        let p = parse(text);
        assert!(p.actions.is_empty(), "no actions: {:?}", p.actions);
        assert!(p.prose.contains("READ_FILE: src/main.rs"));
        assert!(p.prose.contains("WRITE_FILE:"));
        assert!(p.prose.contains("not-a-verb"), "{:?}", p.prose);
    }

    #[test]
    fn unterminated_block_stays_prose() {
        let text = "start\n```tool\nREAD_FILE\npath: a.rs";
        let p = parse(text);
        assert!(p.actions.is_empty());
        assert!(p.prose.contains("```tool"));
        assert!(p.prose.contains("path: a.rs"), "{:?}", p.prose);
    }

    #[test]
    fn block_without_path_is_invalid_not_action() {
        let text = "```tool\nREAD_FILE\n```";
        let p = parse(text);
        assert!(p.actions.is_empty());
        assert_eq!(p.invalid_blocks.len(), 1);
    }

    #[test]
    fn code_fence_in_prose_without_tool_keyword_untouched() {
        let text = "```rust\nfn main() {}\n```";
        let p = parse(text);
        assert!(p.actions.is_empty());
        assert!(p.prose.contains("fn main()"));
    }

    #[test]
    fn crlf_lines() {
        let text = "```tool\r\nREAD_FILE\r\npath: a.rs\r\n```";
        let p = parse(text);
        assert_eq!(p.actions.len(), 1);
    }

    #[test]
    fn garbage_in_params_aborts_to_prose() {
        let text = "before\n```tool\nREAD_FILE\nthis is not a param\n```\nafter";
        let p = parse(text);
        assert!(p.actions.is_empty());
        assert!(p.prose.contains("before"));
        assert!(p.prose.contains("this is not a param"), "{:?}", p.prose);
        assert!(p.prose.contains("after"));
    }

    #[test]
    fn run_command_parses_with_optional_keys() {
        let text = "```tool\nRUN_COMMAND\ncommand: cargo build --release\ncwd: crates/core\ntimeout: 60\n```";
        let p = parse(text);
        assert!(p.invalid_blocks.is_empty(), "{:?}", p.invalid_blocks);
        assert_eq!(p.actions.len(), 1);
        match &p.actions[0] {
            Action::RunCommand {
                command,
                cwd,
                timeout,
            } => {
                assert_eq!(command, "cargo build --release");
                assert_eq!(cwd.as_deref(), Some("crates/core"));
                assert_eq!(*timeout, Some(60));
            }
            other => panic!("wrong action: {other:?}"),
        }
    }

    #[test]
    fn run_command_keeps_colons_in_command() {
        let text = "```tool\nRUN_COMMAND\ncommand: sh -c \"echo a:b\"\n```";
        let p = parse(text);
        assert_eq!(p.actions.len(), 1);
        match &p.actions[0] {
            Action::RunCommand { command, .. } => assert_eq!(command, "sh -c \"echo a:b\""),
            other => panic!("wrong action: {other:?}"),
        }
    }

    #[test]
    fn run_command_without_command_is_invalid() {
        let text = "```tool\nRUN_COMMAND\ncwd: .\n```";
        let p = parse(text);
        assert!(p.actions.is_empty());
        assert_eq!(p.invalid_blocks.len(), 1);
    }

    #[test]
    fn run_command_truncated_label() {
        let long = "x".repeat(100);
        let a = Action::RunCommand {
            command: long,
            cwd: None,
            timeout: None,
        };
        let label = crate::protocol::action_label(&a);
        assert!(label.chars().count() < 80, "{label}");
    }

    #[test]
    fn xml_call_with_inline_params() {
        // real output from openbmb/minicpm5-2b: mixed bogus closing tags
        let p =
            parse("Reading.\n<function name=\"READ_FILE\">path: src/nah.exe</param></function>\n");
        assert_eq!(
            p.actions,
            vec![Action::ReadFile {
                path: "src/nah.exe".into(),
                offset: None,
                limit: None
            }]
        );
        assert!(p.prose.contains("Reading."));
    }

    #[test]
    fn xml_call_unterminated_then_next_call() {
        // first call closes with a bogus tag, second opens right after
        let text = "<function name=\"LIST_DIR\">path: src</name>\n\
                    <function name=\"READ_FILE\">path: a.txt</param></function>";
        let p = parse(text);
        assert_eq!(
            p.actions,
            vec![
                Action::ListDir { path: "src".into() },
                Action::ReadFile {
                    path: "a.txt".into(),
                    offset: None,
                    limit: None
                }
            ]
        );
    }

    #[test]
    fn xml_edit_multiline_with_degenerate_markers() {
        // real output from the log: SEARCH/REPLACE body + repeated closers
        let text = "<function name=\"EDIT_FILE\"> path: hello.txt\n\
                    <<<<<<< SEARCH\nHello, World!\n=======\nGoodbye, world!\n>>>>>>> REPLACE\n=======\n=======\n>>>>>>> REPLACE\n>>>>>>> REPLACE\n";
        let p = parse(text);
        assert_eq!(
            p.actions,
            vec![Action::EditFile {
                path: "hello.txt".into(),
                search: "Hello, World!".into(),
                replace: "Goodbye, world!".into()
            }]
        );
    }

    #[test]
    fn xml_param_tag_form() {
        let p = parse("<function name='LIST_DIR'><param name=\"path\">src</param></function>");
        assert_eq!(p.actions, vec![Action::ListDir { path: "src".into() }]);
    }

    #[test]
    fn fenced_block_with_duplicate_closers_is_valid() {
        let text = "```tool\nEDIT_FILE\npath: a.txt\n<<<<<<< SEARCH\nx\n=======\ny\n>>>>>>> REPLACE\n=======\n>>>>>>> REPLACE\n```";
        let p = parse(text);
        assert_eq!(
            p.actions,
            vec![Action::EditFile {
                path: "a.txt".into(),
                search: "x".into(),
                replace: "y".into()
            }]
        );
        assert!(p.invalid_blocks.is_empty());
    }

    #[test]
    fn xml_block_missing_args_is_invalid_not_executed() {
        let p = parse("<function name=\"READ_FILE\"></function>");
        assert!(p.actions.is_empty());
    }
}
