//! Prompt loading. Mirrors `docs/prompts.md`.
//!
//! Prompt files live in `prompts/` and are embedded at compile time. Each has a
//! custom frontmatter block (`id`, `role`, ...) followed by a plain-markdown
//! body. Callers render the body by substituting `{{var}}` placeholders.

/// A parsed prompt: its `id` and the markdown body (frontmatter stripped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub id: String,
    pub body: String,
}

/// Errors from prompt parsing or rendering.
#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    #[error("prompt is missing its frontmatter block")]
    MissingFrontmatter,
    #[error("prompt frontmatter is missing an id")]
    MissingId,
    #[error("unresolved placeholder {0} after rendering")]
    UnresolvedPlaceholder(String),
}

const PLANNER: &str = include_str!("../../prompts/planner.md");
const INTAKE_QUESTIONS: &str = include_str!("../../prompts/intake-questions.md");
const MERGE: &str = include_str!("../../prompts/merge.md");
const IMPLEMENTER: &str = include_str!("../../prompts/implementer.md");
const REVIEWER: &str = include_str!("../../prompts/reviewer.md");

impl Prompt {
    /// The planner (PL) prompt.
    pub fn planner() -> Prompt {
        Prompt::parse(PLANNER).expect("embedded planner prompt is valid")
    }

    /// The intake-questions (PL) prompt.
    pub fn intake_questions() -> Prompt {
        Prompt::parse(INTAKE_QUESTIONS).expect("embedded intake-questions prompt is valid")
    }

    /// The merge (CO) prompt.
    pub fn merge() -> Prompt {
        Prompt::parse(MERGE).expect("embedded merge prompt is valid")
    }

    /// The implementer (IM) prompt.
    pub fn implementer() -> Prompt {
        Prompt::parse(IMPLEMENTER).expect("embedded implementer prompt is valid")
    }

    /// The reviewer (RV) prompt.
    pub fn reviewer() -> Prompt {
        Prompt::parse(REVIEWER).expect("embedded reviewer prompt is valid")
    }

    /// Parse a prompt file: a `---`-delimited frontmatter block, then the body.
    pub fn parse(text: &str) -> Result<Prompt, PromptError> {
        let rest = text
            .strip_prefix("---\n")
            .ok_or(PromptError::MissingFrontmatter)?;
        let end = rest
            .find("\n---\n")
            .ok_or(PromptError::MissingFrontmatter)?;
        let front = &rest[..end];
        let body = &rest[end + "\n---\n".len()..];

        let id = front
            .lines()
            .find_map(|l| l.strip_prefix("id:").map(str::trim))
            .filter(|s| !s.is_empty())
            .ok_or(PromptError::MissingId)?;

        Ok(Prompt {
            id: id.to_string(),
            body: body.trim_start_matches('\n').to_string(),
        })
    }

    /// Render the body, replacing each `{{key}}` with its value. Errors if any
    /// `{{...}}` placeholder remains unresolved.
    pub fn render(&self, vars: &[(&str, &str)]) -> Result<String, PromptError> {
        let mut out = self.body.clone();
        for (key, value) in vars {
            out = out.replace(&format!("{{{{{key}}}}}"), value);
        }
        if let Some(start) = out.find("{{") {
            if let Some(len) = out[start..].find("}}") {
                let ph = &out[start..start + len + 2];
                return Err(PromptError::UnresolvedPlaceholder(ph.to_string()));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_prompts_parse_with_expected_ids() {
        assert_eq!(Prompt::planner().id, "planner");
        assert_eq!(Prompt::intake_questions().id, "intake-questions");
        assert_eq!(Prompt::merge().id, "merge");
        assert_eq!(Prompt::implementer().id, "implementer");
        assert_eq!(Prompt::reviewer().id, "reviewer");
    }

    #[test]
    fn body_excludes_frontmatter() {
        let p = Prompt::planner();
        assert!(!p.body.contains("model-target"));
        assert!(p.body.contains("# Planner"));
    }

    #[test]
    fn implementer_leaves_commit_ownership_to_coordinator() {
        assert!(Prompt::implementer()
            .body
            .contains("Do not run `git commit`"));
    }

    #[test]
    fn render_substitutes_placeholders() {
        let p = Prompt {
            id: "x".into(),
            body: "hello {{name}}, WI: {{work_item}}".into(),
        };
        let out = p
            .render(&[("name", "world"), ("work_item", "do the thing")])
            .unwrap();
        assert_eq!(out, "hello world, WI: do the thing");
    }

    #[test]
    fn render_rejects_unresolved_placeholder() {
        let p = Prompt {
            id: "x".into(),
            body: "hi {{missing}}".into(),
        };
        assert!(matches!(
            p.render(&[]),
            Err(PromptError::UnresolvedPlaceholder(_))
        ));
    }

    #[test]
    fn parse_rejects_missing_frontmatter() {
        assert!(matches!(
            Prompt::parse("no frontmatter here"),
            Err(PromptError::MissingFrontmatter)
        ));
    }
}
