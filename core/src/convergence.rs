//! Parsing the Coordinator merge output (see `prompts/merge.md`).
//!
//! The merge prompt returns a `## Plan` section and a `## Convergence` line that
//! is either `CONVERGED` or `ITERATE ...`. These helpers extract the plan text
//! and the convergence verdict, plus a line-based diff ratio used to apply the
//! configured convergence threshold as a fallback signal.

/// The parsed result of a merge invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge {
    /// The merged plan (the `## Plan` section, or the whole output as a fallback).
    pub plan: String,
    /// Whether the model reported `CONVERGED`.
    pub converged: bool,
}

/// Parse a merge agent's output into a plan and a convergence verdict.
pub fn parse_merge(output: &str) -> Merge {
    let plan = extract_section(output, "## Plan").unwrap_or_else(|| output.trim().to_string());
    let converged = match extract_section(output, "## Convergence") {
        Some(section) => section
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|first| first.to_ascii_uppercase().starts_with("CONVERGED"))
            .unwrap_or(false),
        None => false,
    };
    Merge { plan, converged }
}

/// Extract the body of a `## Heading` section: the lines after the heading up to
/// the next `## ` heading (or end of input). Returns `None` if not present.
fn extract_section(text: &str, heading: &str) -> Option<String> {
    let mut lines = text.lines();
    let mut body: Vec<&str> = Vec::new();
    // Advance to the heading.
    for line in lines.by_ref() {
        if line.trim() == heading {
            break;
        }
    }
    // Collect until the next level-2 heading.
    let mut found_any = false;
    for line in lines {
        if line.trim_start().starts_with("## ") {
            break;
        }
        found_any = true;
        body.push(line);
    }
    if !found_any && !text.lines().any(|l| l.trim() == heading) {
        return None;
    }
    Some(body.join("\n").trim().to_string())
}

/// A line-based difference ratio in `[0.0, 1.0]`: `0.0` when the two texts have
/// the same set of lines (order-insensitive multiset), `1.0` when they share
/// none. Used to decide whether a merged plan is "materially unchanged".
pub fn diff_ratio(a: &str, b: &str) -> f64 {
    let la: Vec<&str> = a.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let lb: Vec<&str> = b.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if la.is_empty() && lb.is_empty() {
        return 0.0;
    }
    // Multiset intersection size.
    let mut remaining: Vec<&str> = lb.clone();
    let mut common = 0usize;
    for line in &la {
        if let Some(pos) = remaining.iter().position(|r| r == line) {
            remaining.remove(pos);
            common += 1;
        }
    }
    let changed = (la.len() - common) + (lb.len() - common);
    let denom = la.len() + lb.len();
    changed as f64 / denom as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONVERGED_OUTPUT: &str =
        "## Plan\nSummary: do X.\n\nSteps:\n1. a\n\n## Convergence\nCONVERGED";
    const ITERATE_OUTPUT: &str =
        "## Plan\nSummary: do Y.\n\n## Convergence\nITERATE — step order still differs";

    #[test]
    fn parses_converged() {
        let m = parse_merge(CONVERGED_OUTPUT);
        assert!(m.converged);
        assert!(m.plan.contains("do X"));
        assert!(!m.plan.contains("Convergence"));
    }

    #[test]
    fn parses_iterate() {
        let m = parse_merge(ITERATE_OUTPUT);
        assert!(!m.converged);
        assert!(m.plan.contains("do Y"));
    }

    #[test]
    fn missing_convergence_section_is_not_converged() {
        let m = parse_merge("## Plan\njust a plan");
        assert!(!m.converged);
        assert_eq!(m.plan, "just a plan");
    }

    #[test]
    fn no_plan_section_falls_back_to_whole_output() {
        let m = parse_merge("raw text with no headings");
        assert_eq!(m.plan, "raw text with no headings");
    }

    #[test]
    fn diff_ratio_identical_is_zero() {
        assert_eq!(diff_ratio("a\nb\nc", "a\nb\nc"), 0.0);
    }

    #[test]
    fn diff_ratio_disjoint_is_one() {
        assert_eq!(diff_ratio("a\nb", "c\nd"), 1.0);
    }

    #[test]
    fn diff_ratio_partial_is_between() {
        // a,b,c vs a,b,d: one changed on each side out of six total lines.
        let r = diff_ratio("a\nb\nc", "a\nb\nd");
        assert!(r > 0.0 && r < 1.0);
    }
}
