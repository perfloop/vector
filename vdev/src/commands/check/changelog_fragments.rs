use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::commands::changelog::new::TODO_HANDLE;
use crate::utils::{git, paths};

const CHANGELOG_DIR: &str = "changelog.d";
const DEFAULT_MAX_FRAGMENTS: usize = 1000;

/// Allowed changelog fragment types.
///
/// NOTE: keep this list in sync with `vdev/src/commands/release/generate_cue.rs`
/// and `changelog.d/README.md`.
const FRAGMENT_TYPES: &[&str] = &["breaking", "security", "feature", "enhancement", "fix"];

/// Validate changelog fragments added on this branch/PR.
#[derive(clap::Args, Debug)]
#[command()]
pub struct Cli {
    /// Merge base to diff against.
    #[arg(long, default_value = "origin/master")]
    merge_base: String,

    /// Maximum number of fragments accepted in a single PR.
    #[arg(long, default_value_t = DEFAULT_MAX_FRAGMENTS)]
    max_fragments: usize,
}

impl Cli {
    pub fn exec(self) -> Result<()> {
        let repo_root = paths::find_repo_root()?;
        let changelog_dir = repo_root.join(CHANGELOG_DIR);
        if !changelog_dir.is_dir() {
            bail!(
                "No {CHANGELOG_DIR}/ directory at {}. Run this from the Vector repo root.",
                repo_root.display()
            );
        }

        let fragments = added_fragments(&self.merge_base)?;
        if fragments.is_empty() {
            bail!(
                "No changelog fragments detected. \
                 If no changes necessitate user-facing explanations, add the 'no-changelog' label. \
                 Otherwise, add fragments to {CHANGELOG_DIR}/ (see {CHANGELOG_DIR}/README.md)."
            );
        }
        if fragments.len() > self.max_fragments {
            bail!(
                "Too many changelog fragments ({} > {}).",
                fragments.len(),
                self.max_fragments
            );
        }

        let expected_parent = std::path::Path::new(CHANGELOG_DIR);
        for path in &fragments {
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                bail!("Unexpected fragment path: {}", path.display());
            };
            if name == "README.md" {
                continue;
            }
            if path.parent() != Some(expected_parent) {
                bail!(
                    "invalid fragment path '{}': fragments must live directly under {CHANGELOG_DIR}/, not in a subdirectory.",
                    path.display()
                );
            }
            info!("Validating '{name}'");
            let fragment_type = validate_filename(name)?;
            validate_contents(&repo_root.join(path), name, fragment_type)?;
        }

        info!("changelog additions are valid.");
        Ok(())
    }
}

/// `git diff --name-only --diff-filter=A --merge-base <merge_base> changelog.d`
fn added_fragments(merge_base: &str) -> Result<Vec<PathBuf>> {
    let out = git::run_and_check_output(&[
        "diff",
        "--name-only",
        "--diff-filter=A",
        "--merge-base",
        merge_base,
        CHANGELOG_DIR,
    ])?;
    Ok(out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(PathBuf::from)
        .collect())
}

fn validate_filename(filename: &str) -> Result<&'static str> {
    let parts: Vec<&str> = filename.split('.').collect();
    if parts.len() != 3 {
        bail!(
            "invalid fragment filename '{filename}': expected '<unique_name>.<fragment_type>.md'"
        );
    }
    let fragment_type = parts[1];
    let Some(known) = FRAGMENT_TYPES.iter().find(|t| **t == fragment_type) else {
        bail!(
            "invalid fragment filename '{filename}': fragment type must be one of ({}).",
            FRAGMENT_TYPES.join("|")
        );
    };
    if parts[2] != "md" {
        bail!("invalid fragment filename '{filename}': extension must be markdown (.md).");
    }
    Ok(*known)
}

fn validate_contents(path: &std::path::Path, filename: &str, fragment_type: &str) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    // Match generate_cue.rs, which reads `lines().last()` verbatim: the authors
    // line must be the last line, no trailing blank lines allowed.
    let last_line = content.lines().last().unwrap_or("");

    let Some(names) = last_line.strip_prefix("authors: ") else {
        bail!(
            "invalid fragment contents for '{filename}': last line must be 'authors: <name> [<name> ...]' (no trailing blank lines)."
        );
    };
    let names = names.trim();
    if names.is_empty() {
        bail!("invalid fragment contents for '{filename}': authors line has no names.");
    }
    if names.contains('@') {
        bail!(
            "invalid fragment contents for '{filename}': author names should not be prefixed with '@'."
        );
    }
    if names.contains(',') {
        bail!(
            "invalid fragment contents for '{filename}': authors should be space delimited, not comma delimited."
        );
    }
    if names.split_whitespace().any(|n| n == TODO_HANDLE) {
        bail!(
            "invalid fragment contents for '{filename}': the scaffolder placeholder '{TODO_HANDLE}' must be replaced with a real GitHub handle."
        );
    }

    if fragment_type == "breaking" {
        validate_breaking_fragment(&content, filename)?;
    }

    Ok(())
}

/// Frontmatter for `*.breaking.md`.
///
/// Full file layout:
///
/// ```text
/// ---
/// title: "..."
/// anchor: "..."   # optional
/// ---
///
/// ## Summary
///
/// <markdown — one paragraph, lands in the release changelog list>
///
/// ## Migration
///
/// <markdown — "Action needed" body for the upgrade guide, or "N/A">
///
/// authors: <name> [<name> ...]
/// ```
///
/// Prose lives under headers so it can be plain markdown (fenced code blocks, sub-
/// headings, lists) without YAML block-scalar indentation gymnastics. Keep this in
/// sync with `changelog.d/README.md`.
#[derive(Deserialize, Debug)]
struct BreakingFrontmatter {
    /// Section heading in the generated upgrade guide.
    title: String,
    /// Stable anchor slug. Optional — the generator derives one from `title` when omitted.
    #[serde(default)]
    anchor: Option<String>,
}

const SUMMARY_HEADER: &str = "## Summary";
const MIGRATION_HEADER: &str = "## Migration";

fn validate_breaking_fragment(content: &str, filename: &str) -> Result<()> {
    let Some(rest) = content.strip_prefix("---\n") else {
        bail!(
            "invalid breaking fragment '{filename}': must begin with a YAML frontmatter block (see changelog.d/README.md)."
        );
    };
    let Some((frontmatter_yaml, body)) = rest.split_once("\n---\n") else {
        bail!(
            "invalid breaking fragment '{filename}': missing closing '---' for the frontmatter block."
        );
    };

    let fm: BreakingFrontmatter = serde_yaml::from_str(frontmatter_yaml)
        .with_context(|| format!("invalid breaking fragment '{filename}': frontmatter YAML"))?;

    if fm.title.trim().is_empty() {
        bail!("invalid breaking fragment '{filename}': `title` must not be empty.");
    }
    if fm.title.contains("TODO") {
        bail!(
            "invalid breaking fragment '{filename}': `title` still contains the scaffolder placeholder 'TODO'."
        );
    }
    if let Some(anchor) = &fm.anchor
        && !is_valid_anchor(anchor)
    {
        bail!(
            "invalid breaking fragment '{filename}': `anchor` must be a lowercase kebab-case slug (a-z, 0-9, hyphens); got '{anchor}'."
        );
    }

    // Body — up to but not including the trailing `authors:` line — must contain
    // `## Summary` then `## Migration`, each with non-empty content.
    let body_before_authors: String = body
        .lines()
        .take_while(|l| !l.starts_with("authors: "))
        .collect::<Vec<_>>()
        .join("\n");

    let summary_start = find_header(&body_before_authors, SUMMARY_HEADER).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid breaking fragment '{filename}': missing `{SUMMARY_HEADER}` section."
        )
    })?;
    let migration_start = find_header(&body_before_authors, MIGRATION_HEADER).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid breaking fragment '{filename}': missing `{MIGRATION_HEADER}` section."
        )
    })?;
    if migration_start < summary_start {
        bail!(
            "invalid breaking fragment '{filename}': `{SUMMARY_HEADER}` must come before `{MIGRATION_HEADER}`."
        );
    }

    // Offsets came from `find_header`, which matched the ASCII header at that byte position,
    // so `.get(..)` will always return Some here. Using `.get()` keeps clippy::string_slice quiet.
    let summary_body = body_before_authors
        .get(summary_start + SUMMARY_HEADER.len()..migration_start)
        .unwrap_or("")
        .trim();
    if summary_body.is_empty() {
        bail!("invalid breaking fragment '{filename}': `{SUMMARY_HEADER}` section is empty.");
    }
    let migration_body = body_before_authors
        .get(migration_start + MIGRATION_HEADER.len()..)
        .unwrap_or("")
        .trim();
    if migration_body.is_empty() {
        bail!(
            "invalid breaking fragment '{filename}': `{MIGRATION_HEADER}` section is empty (use 'N/A' for informational breakers)."
        );
    }

    Ok(())
}

/// Match a header only when it starts at column 0 AND is not inside a fenced code block.
/// Treats any line starting with three backticks as a fence toggle.
fn find_header(body: &str, header: &str) -> Option<usize> {
    let mut offset = 0;
    let mut in_fence = false;
    for line in body.split_inclusive('\n') {
        let trimmed_end = line.trim_end_matches('\n');
        if trimmed_end.starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence && trimmed_end == header {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

fn is_valid_anchor(anchor: &str) -> bool {
    !anchor.is_empty()
        && anchor
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !anchor.starts_with('-')
        && !anchor.ends_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::{formatdoc, indoc};

    /// Build a full breaking-fragment file body from the given frontmatter and section content.
    fn wrap(frontmatter: &str, summary: &str, migration: &str) -> String {
        formatdoc! {"
            ---
            {frontmatter}
            ---

            ## Summary

            {summary}

            ## Migration

            {migration}

            authors: pront
        "}
    }

    #[test]
    fn valid_breaking_fragment() {
        let raw = wrap(
            indoc! {r#"
                title: "Env var interpolation disabled"
                anchor: env-var
            "#}
            .trim(),
            "Off by default now.",
            "Pass the flag.",
        );
        validate_breaking_fragment(&raw, "x.breaking.md").unwrap();
    }

    #[test]
    fn valid_without_anchor() {
        let raw = wrap(r#"title: "A change""#, "Change happened.", "N/A");
        validate_breaking_fragment(&raw, "x.breaking.md").unwrap();
    }

    #[test]
    fn missing_frontmatter() {
        let raw = indoc! {"
            no frontmatter here

            authors: pront
        "};
        let err = validate_breaking_fragment(raw, "x.breaking.md").unwrap_err();
        assert!(err.to_string().contains("YAML frontmatter"), "{err}");
    }

    #[test]
    fn missing_closing_delimiter() {
        let raw = indoc! {"
            ---
            title: x

            ## Summary

            hi

            authors: pront
        "};
        let err = validate_breaking_fragment(raw, "x.breaking.md").unwrap_err();
        assert!(err.to_string().contains("closing '---'"), "{err}");
    }

    #[test]
    fn empty_title() {
        let raw = wrap(r#"title: """#, "x", "N/A");
        let err = validate_breaking_fragment(&raw, "x.breaking.md").unwrap_err();
        assert!(
            err.to_string().contains("`title` must not be empty"),
            "{err}"
        );
    }

    #[test]
    fn missing_summary_header() {
        // Handwritten fragment with only ## Migration.
        let raw = indoc! {r#"
            ---
            title: "x"
            ---

            ## Migration

            N/A

            authors: pront
        "#};
        let err = validate_breaking_fragment(raw, "x.breaking.md").unwrap_err();
        assert!(err.to_string().contains("missing `## Summary`"), "{err}");
    }

    #[test]
    fn missing_migration_header() {
        let raw = indoc! {r#"
            ---
            title: "x"
            ---

            ## Summary

            y

            authors: pront
        "#};
        let err = validate_breaking_fragment(raw, "x.breaking.md").unwrap_err();
        assert!(err.to_string().contains("missing `## Migration`"), "{err}");
    }

    #[test]
    fn empty_summary_section() {
        let raw = wrap(r#"title: "x""#, "", "N/A");
        let err = validate_breaking_fragment(&raw, "x.breaking.md").unwrap_err();
        assert!(
            err.to_string().contains("`## Summary` section is empty"),
            "{err}"
        );
    }

    #[test]
    fn empty_migration_section() {
        let raw = wrap(r#"title: "x""#, "y", "");
        let err = validate_breaking_fragment(&raw, "x.breaking.md").unwrap_err();
        assert!(
            err.to_string().contains("`## Migration` section is empty"),
            "{err}"
        );
    }

    #[test]
    fn wrong_section_order() {
        let raw = indoc! {r#"
            ---
            title: "x"
            ---

            ## Migration

            do this

            ## Summary

            hi

            authors: pront
        "#};
        let err = validate_breaking_fragment(raw, "x.breaking.md").unwrap_err();
        assert!(err.to_string().contains("must come before"), "{err}");
    }

    #[test]
    fn bad_anchor() {
        let raw = wrap(
            indoc! {r#"
                title: "x"
                anchor: "Not Valid!"
            "#}
            .trim(),
            "y",
            "N/A",
        );
        let err = validate_breaking_fragment(&raw, "x.breaking.md").unwrap_err();
        assert!(err.to_string().contains("kebab-case slug"), "{err}");
    }

    #[test]
    fn todo_title_rejected() {
        let raw = wrap(r#"title: "TODO one-line title""#, "y", "N/A");
        let err = validate_breaking_fragment(&raw, "x.breaking.md").unwrap_err();
        assert!(err.to_string().contains("placeholder 'TODO'"), "{err}");
    }

    #[test]
    fn headers_inside_code_fence_are_ignored() {
        // `## Summary` / `## Migration` only appear inside a fenced code block.
        let raw = indoc! {r#"
            ---
            title: "x"
            ---

            ```
            ## Summary
            fake

            ## Migration
            fake
            ```

            authors: pront
        "#};
        let err = validate_breaking_fragment(raw, "x.breaking.md").unwrap_err();
        assert!(err.to_string().contains("missing `## Summary`"), "{err}");
    }
}
