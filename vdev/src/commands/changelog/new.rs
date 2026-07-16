use std::fs;
use std::process::Command;

use anyhow::{Result, bail};
use indoc::formatdoc;

use crate::utils::paths;

const CHANGELOG_DIR: &str = "changelog.d";
const FRAGMENT_TYPES: &[&str] = &["breaking", "security", "feature", "enhancement", "fix"];

/// Scaffold a new changelog fragment.
#[derive(clap::Args, Debug)]
#[command()]
pub struct Cli {
    /// Fragment type.
    #[arg(value_parser = clap::builder::PossibleValuesParser::new(FRAGMENT_TYPES))]
    fragment_type: String,
    /// Unique slug (used as the filename prefix, e.g. `env_var_interpolation`).
    slug: String,
}

impl Cli {
    pub fn exec(self) -> Result<()> {
        let repo_root = paths::find_repo_root()?;
        let dir = repo_root.join(CHANGELOG_DIR);
        if !dir.is_dir() {
            bail!("{} does not exist", dir.display());
        }
        let file = dir.join(format!("{}.{}.md", self.slug, self.fragment_type));
        if file.exists() {
            bail!("{} already exists", file.display());
        }

        let author = detect_gh_handle().unwrap_or_else(|| "TODO_your_gh_handle".to_string());
        let content = render_template(&self.fragment_type, &author);
        fs::write(&file, content)?;

        info!("Created {}", relative(&repo_root, &file).display());
        info!("Edit the file, then run `make check-changelog-fragments` to validate.");
        Ok(())
    }
}

fn render_template(fragment_type: &str, author: &str) -> String {
    match fragment_type {
        "breaking" => formatdoc! {r#"
            ---
            title: "TODO one-line title"
            migration: |
              TODO how to migrate. Use "N/A" for informational-only breakers.

              For config changes, show a before/after with fenced code blocks, e.g.:

              ##### Old

              ```yaml
              sinks:
                my_sink:
                  option: old_value
              ```

              ##### New

              ```yaml
              sinks:
                my_sink:
                  option: new_value
              ```
            ---
            TODO one-paragraph summary that lands in the changelog list.

            authors: {author}
        "#},
        _ => formatdoc! {"
            TODO one-item description of the change.

            authors: {author}
        "},
    }
}

/// Best-effort GitHub handle detection:
/// 1. `git config --get github.user` (explicit override)
/// 2. `gh api user --jq .login` (uses the authenticated `gh` CLI if installed)
/// 3. `git config user.email` when it matches `<id>+<handle>@users.noreply.github.com`
///    or `<handle>@users.noreply.github.com`
fn detect_gh_handle() -> Option<String> {
    if let Some(h) = git_config("github.user") {
        return Some(h);
    }
    if let Some(h) = gh_authenticated_login() {
        return Some(h);
    }
    let email = git_config("user.email")?;
    handle_from_noreply(&email)
}

fn git_config(key: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--get", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

fn gh_authenticated_login() -> Option<String> {
    let out = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

fn handle_from_noreply(email: &str) -> Option<String> {
    let local = email
        .split_once('@')
        .filter(|(_, d)| *d == "users.noreply.github.com")?
        .0;
    let handle = local.split_once('+').map_or(local, |(_, h)| h);
    (!handle.is_empty()).then(|| handle.to_string())
}

fn relative<'a>(base: &std::path::Path, path: &'a std::path::Path) -> &'a std::path::Path {
    path.strip_prefix(base).unwrap_or(path)
}
