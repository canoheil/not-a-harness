//! Tiny line-based diff for confirmation previews.
//!
//! Classic LCS over lines; inputs are capped so pathological files can't
//! make the O(n·m) table explode — beyond the cap we fall back to a
//! before/after summary.

const LCS_CAP: usize = 1500;
const CONTEXT: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Keep,
    Del,
    Add,
}

/// (op, rendered line) for the whole file pair.
fn diff_marks(a: &[&str], b: &[&str]) -> Vec<(Op, String)> {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut marks = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            marks.push((Op::Keep, format!("  {}", a[i])));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            marks.push((Op::Del, format!("- {}", a[i])));
            i += 1;
        } else {
            marks.push((Op::Add, format!("+ {}", b[j])));
            j += 1;
        }
    }
    while i < n {
        marks.push((Op::Del, format!("- {}", a[i])));
        i += 1;
    }
    while j < m {
        marks.push((Op::Add, format!("+ {}", b[j])));
        j += 1;
    }
    marks
}

/// Render a human-readable unified-ish diff between `old` and `new`.
pub fn render(old: &str, new: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();

    if a.len() > LCS_CAP || b.len() > LCS_CAP {
        return format!(
            "(file too large for a line diff: {} -> {} lines)\n\
             --- first lines of old ---\n{}\n--- first lines of new ---\n{}",
            a.len(),
            b.len(),
            a.iter().take(10).cloned().collect::<Vec<_>>().join("\n"),
            b.iter().take(10).cloned().collect::<Vec<_>>().join("\n"),
        );
    }

    let marks = diff_marks(&a, &b);

    // Which indices to print: every change plus CONTEXT lines around it.
    let mut show = vec![false; marks.len()];
    for (idx, (op, _)) in marks.iter().enumerate() {
        if *op != Op::Keep {
            let lo = idx.saturating_sub(CONTEXT);
            let hi = (idx + CONTEXT + 1).min(marks.len());
            for s in show[lo..hi].iter_mut() {
                *s = true;
            }
        }
    }

    let mut out = String::new();
    let mut last_printed: Option<usize> = None;
    let first_shown = show.iter().position(|s| *s);
    let last_shown = show.iter().rposition(|s| *s);
    if let Some(0) = first_shown {
    } else if first_shown.is_some() {
        out.push_str("  ...\n");
    }
    for (idx, (_, line)) in marks.iter().enumerate() {
        if show[idx] {
            if let Some(prev) = last_printed {
                if idx > prev + 1 {
                    out.push_str("  ...\n");
                }
            }
            out.push_str(line);
            out.push('\n');
            last_printed = Some(idx);
        }
    }
    if let Some(last) = last_shown {
        if last + 1 < marks.len() {
            out.push_str("  ...\n");
        }
    }
    if out.is_empty() {
        out.push_str("(no line changes)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_change() {
        let d = render("a\nb\nc\n", "a\nB\nc\n");
        assert!(d.contains("- b"), "{d}");
        assert!(d.contains("+ B"), "{d}");
        assert!(d.contains("  a"), "{d}");
        assert!(d.contains("  c"), "{d}");
    }

    #[test]
    fn pure_add() {
        let d = render("a\n", "a\nb\n");
        assert!(d.contains("+ b"), "{d}");
        assert!(!d.contains("- "), "{d}");
    }

    #[test]
    fn empty_old() {
        let d = render("", "hello\n");
        assert!(d.contains("+ hello"), "{d}");
    }

    #[test]
    fn gap_elision() {
        let old = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let new = old.replace("line 20\n", "LINE 20\n");
        let d = render(&old, &new);
        assert!(d.contains("..."), "{d}");
        assert!(d.contains("- line 20"), "{d}");
        assert!(d.contains("+ LINE 20"), "{d}");
    }
}
