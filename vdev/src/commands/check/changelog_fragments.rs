use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

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

    if fragment_type == "breaking" {
        validate_breaking_fragment(&content, filename)?;
    }

    Ok(())
}

/// Frontmatter schema for `*.breaking.md`.
///
/// The full file layout is:
///
/// ```text
/// ---
/// <yaml frontmatter>
/// ---
/// <markdown body — one-item summary for the changelog>
///
/// authors: <name> [<name> ...]
/// ```
///
/// Frontmatter fields feed the release-time upgrade guide generator; keep them in sync
/// with the `changelog.d/README.md` "Breaking changes" section.
#[derive(Deserialize, Debug)]
struct BreakingFrontmatter {
    /// Section heading in the generated upgrade guide.
    title: String,
    /// Stable anchor slug. Optional — the generator derives one from `title` when omitted.
    #[serde(default)]
    anchor: Option<String>,
    /// "Action needed" body in markdown. Use the literal string `"N/A"` when there's nothing to do.
    migration: String,
}

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
    if fm.migration.trim().is_empty() {
        bail!(
            "invalid breaking fragment '{filename}': `migration` must not be empty (use \"N/A\" for informational breakers)."
        );
    }
    if let Some(anchor) = &fm.anchor
        && !is_valid_anchor(anchor)
    {
        bail!(
            "invalid breaking fragment '{filename}': `anchor` must be a lowercase kebab-case slug (a-z, 0-9, hyphens); got '{anchor}'."
        );
    }

    // Body between frontmatter and the trailing `authors:` line must be non-empty markdown.
    let body_without_authors: Vec<&str> = body
        .lines()
        .take_while(|l| !l.starts_with("authors: "))
        .collect();
    let body_text = body_without_authors.join("\n");
    if body_text.trim().is_empty() {
        bail!(
            "invalid breaking fragment '{filename}': body between frontmatter and the `authors:` line must not be empty."
        );
    }

    Ok(())
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

    fn wrap(frontmatter: &str, body: &str) -> String {
        format!("---\n{frontmatter}\n---\n{body}\n\nauthors: pront\n")
    }

    #[test]
    fn valid_breaking_fragment() {
        let raw = wrap(
            "title: \"Env var interpolation disabled\"\nanchor: env-var\nmigration: |\n  Pass the flag.",
            "Env var interpolation is off by default.",
        );
        validate_breaking_fragment(&raw, "x.breaking.md").unwrap();
    }

    #[test]
    fn valid_without_anchor() {
        let raw = wrap(
            "title: \"A change\"\nmigration: \"N/A\"",
            "Informational only.",
        );
        validate_breaking_fragment(&raw, "x.breaking.md").unwrap();
    }

    #[test]
    fn missing_frontmatter() {
        let raw = "no frontmatter here\n\nauthors: pront\n";
        let err = validate_breaking_fragment(raw, "x.breaking.md").unwrap_err();
        assert!(err.to_string().contains("YAML frontmatter"), "{err}");
    }

    #[test]
    fn missing_closing_delimiter() {
        let raw = "---\ntitle: x\nmigration: y\n\nbody\n\nauthors: pront\n";
        let err = validate_breaking_fragment(raw, "x.breaking.md").unwrap_err();
        assert!(err.to_string().contains("closing '---'"), "{err}");
    }

    #[test]
    fn empty_title() {
        let raw = wrap("title: \"\"\nmigration: \"N/A\"", "body");
        let err = validate_breaking_fragment(&raw, "x.breaking.md").unwrap_err();
        assert!(
            err.to_string().contains("`title` must not be empty"),
            "{err}"
        );
    }

    #[test]
    fn missing_migration() {
        let raw = wrap("title: \"x\"", "body");
        let err = format!(
            "{:#}",
            validate_breaking_fragment(&raw, "x.breaking.md").unwrap_err()
        );
        assert!(err.contains("migration"), "{err}");
    }

    #[test]
    fn empty_body() {
        let raw = wrap("title: \"x\"\nmigration: \"N/A\"", "");
        let err = validate_breaking_fragment(&raw, "x.breaking.md").unwrap_err();
        assert!(
            err.to_string().contains("body between frontmatter"),
            "{err}"
        );
    }

    #[test]
    fn bad_anchor() {
        let raw = wrap(
            "title: \"x\"\nanchor: \"Not Valid!\"\nmigration: \"N/A\"",
            "body",
        );
        let err = validate_breaking_fragment(&raw, "x.breaking.md").unwrap_err();
        assert!(err.to_string().contains("kebab-case slug"), "{err}");
    }
}
