//! Project memory scratchpad: `.nah/memory.md` inside the project root.
//!
//! The model maintains the file with the ordinary file tools; whenever it
//! exists its content is pinned into the system prompt as a
//! `[PROJECT MEMORY]` block, so durable project facts survive context
//! compaction and small context windows. The block is capped so it can
//! never crowd out the working set.

use std::path::{Path, PathBuf};

/// Relative location of the memory file inside the project root.
pub const MEMORY_REL: &str = ".nah/memory.md";

/// Hard caps for the pinned block (a 4K context cannot afford a novel).
const MAX_LINES: usize = 60;
const MAX_CHARS: usize = 2200;

/// Absolute path of the project memory file.
pub fn memory_path(root: &Path) -> PathBuf {
    root.join(".nah").join("memory.md")
}

/// The `[PROJECT MEMORY]` block for the system prompt, or `None` when the
/// project has no memory file (or it is empty).
pub fn load_block(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(memory_path(root)).ok()?;
    let text = text.trim_end();
    if text.trim().is_empty() {
        return None;
    }
    let mut body = String::new();
    let mut truncated = false;
    for line in text.lines() {
        if body.lines().count() >= MAX_LINES || body.len() + line.len() + 1 > MAX_CHARS {
            truncated = true;
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    let suffix = if truncated {
        "\n(memory file truncated to fit the context window)\n"
    } else {
        "\n"
    };
    Some(format!(
        "[PROJECT MEMORY - .nah/memory.md, durable project notes you maintain]\n\
         {body}{suffix}\
         [END PROJECT MEMORY]"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_or_empty_file_yields_no_block() {
        let r = root("nah_test_mem_empty");
        assert!(load_block(&r).is_none());
        std::fs::create_dir_all(r.join(".nah")).unwrap();
        std::fs::write(memory_path(&r), "  \n\n").unwrap();
        assert!(load_block(&r).is_none());
    }

    #[test]
    fn block_contains_content_and_end_marker() {
        let r = root("nah_test_mem_block");
        std::fs::create_dir_all(r.join(".nah")).unwrap();
        std::fs::write(memory_path(&r), "build: run `x`\nconvention: y").unwrap();
        let block = load_block(&r).unwrap();
        assert!(block.starts_with("[PROJECT MEMORY"));
        assert!(block.contains("build: run `x`"));
        assert!(block.contains("convention: y"));
        assert!(block.contains("[END PROJECT MEMORY]"));
    }

    #[test]
    fn long_memory_is_truncated_with_marker() {
        let r = root("nah_test_mem_trunc");
        std::fs::create_dir_all(r.join(".nah")).unwrap();
        let big = "x".repeat(120) + "\n";
        std::fs::write(memory_path(&r), big.repeat(100)).unwrap();
        let block = load_block(&r).unwrap();
        assert!(block.contains("memory file truncated"));
        assert!(block.len() < 3000);
        assert!(block.contains("[END PROJECT MEMORY]"));
    }
}
