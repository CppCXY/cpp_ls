//! **Set every version in this workspace from a git tag**, for the release build.
//!
//! ```text
//! cargo run -p edit_version -- refs/tags/v0.1.0
//! ```
//!
//! The build workflow runs this before `cargo build` on a tag, so the binary and the published crates carry the tag's
//! version rather than whatever was committed last. Two things are rewritten, and nothing else:
//!
//! * `version = "…"` in every `crates/*/Cargo.toml` — the crate's own version;
//! * the `version` on each of the three published crates in the root `[workspace.dependencies]` table. Without it a
//!   `path` dependency has no version to publish against, and `cargo publish` refuses.
//!
//! The tag is the source of truth: `refs/tags/v1.2.3` and `v1.2.3` both mean `1.2.3`, and a ref that is not a version
//! is an error rather than a silently unchanged tree.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// **The crates whose `version` in `[workspace.dependencies]` has to match the tag.** `edit_version` and the
/// tools are not published, so they are not listed.
const PUBLISHED: [&str; 4] = ["cpp_parser", "cpp_formatter", "cpp_code_analysis", "cpp_ls"];

fn main() -> ExitCode {
    let Some(reference) = std::env::args().nth(1) else {
        eprintln!("usage: edit_version <tag or ref>");
        return ExitCode::FAILURE;
    };
    let Some(version) = version_of(&reference) else {
        eprintln!("`{reference}` is not a version: expected `v1.2.3` or `refs/tags/v1.2.3`");
        return ExitCode::FAILURE;
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/edit_version is two levels under the workspace")
        .to_path_buf();

    let mut changed = 0;
    match rewrite_the_crates(&root, &version) {
        Ok(count) => changed += count,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    }
    match rewrite_the_workspace(&root, &version) {
        Ok(true) => changed += 1,
        Ok(false) => {}
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    }

    println!("version {version}: {changed} manifest(s) rewritten");
    ExitCode::SUCCESS
}

/// **The version a tag names**, or `None` when it names something else.
///
/// `refs/tags/` is stripped first because a workflow sees the full ref, and the `v` because a tag is conventionally
/// spelled `v1.2.3` while the version is `1.2.3`. A trailing pre-release (`-beta.1`) is kept: it is part of the
/// version, and a crate published under it is a pre-release of its own.
fn version_of(reference: &str) -> Option<String> {
    let name = reference.strip_prefix("refs/tags/").unwrap_or(reference);
    let name = name.strip_prefix('v').unwrap_or(name);
    let version = name.split(['-', '+']).next()?;
    let mut parts = version.split('.');
    let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    if [major, minor, patch]
        .iter()
        .any(|part| part.is_empty() || !part.chars().all(|character| character.is_ascii_digit()))
    {
        return None;
    }
    Some(name.to_string())
}

/// **Rewrite `version = "…"` in every crate manifest**, answering how many changed.
fn rewrite_the_crates(root: &Path, version: &str) -> Result<usize, String> {
    let mut changed = 0;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root.join("crates"))
        .map_err(|error| format!("cannot list `crates/`: {error}"))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("Cargo.toml"))
        .filter(|manifest| manifest.is_file())
        .collect();
    entries.sort();
    for manifest in entries {
        let text = std::fs::read_to_string(&manifest)
            .map_err(|error| format!("cannot read `{}`: {error}", manifest.display()))?;
        let Some(rewritten) = with_the_crate_version(&text, version) else {
            eprintln!("{} has no `version`", manifest.display());
            continue;
        };
        if rewritten != text {
            std::fs::write(&manifest, rewritten)
                .map_err(|error| format!("cannot write `{}`: {error}", manifest.display()))?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// **The manifest with its `[package] version` set** — the first `version = "…"` line in the file, which is the one
/// `[package]` owns: a dependency's version is written either as `name.version = true` or in the workspace table.
fn with_the_crate_version(text: &str, version: &str) -> Option<String> {
    let mut done = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if !done && trimmed.starts_with("version") && trimmed.contains('=') {
            let indent = &line[..line.len() - line.trim_start().len()];
            out.push_str(&format!("{indent}version = \"{version}\"\n"));
            done = true;
            continue;
        }
        out.push_str(line);
    }
    done.then_some(out)
}

/// **Rewrite the `version` on the published crates in `[workspace.dependencies]`**, answering whether the file
/// changed.
fn rewrite_the_workspace(root: &Path, version: &str) -> Result<bool, String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|error| format!("cannot read `{}`: {error}", manifest.display()))?;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        match a_published_crate_on(line) {
            Some(name) => {
                let indent = &line[..line.len() - line.trim_start().len()];
                out.push_str(&format!(
                    "{indent}{name} = {{ path = \"crates/{name}\", version = \"{version}\" }}\n"
                ));
            }
            None => out.push_str(line),
        }
    }
    if out == text {
        return Ok(false);
    }
    std::fs::write(&manifest, out)
        .map_err(|error| format!("cannot write `{}`: {error}", manifest.display()))?;
    Ok(true)
}

/// **Which published crate a `[workspace.dependencies]` line names**, if it names one.
fn a_published_crate_on(line: &str) -> Option<&'static str> {
    let trimmed = line.trim_start();
    PUBLISHED
        .iter()
        .find(|name| {
            trimmed.starts_with(&format!("{name} = ")) || trimmed.starts_with(&format!("{name}="))
        })
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_names_a_version() {
        assert_eq!(version_of("refs/tags/v0.1.0").as_deref(), Some("0.1.0"));
        assert_eq!(version_of("v0.1.0").as_deref(), Some("0.1.0"));
        assert_eq!(version_of("0.1.0").as_deref(), Some("0.1.0"));
        assert_eq!(version_of("v1.2.3-beta.1").as_deref(), Some("1.2.3-beta.1"));
    }

    /// A tag that is not a version is **an error, not a no-op**: a release that silently kept the old version is the
    /// one failure this tool exists to prevent.
    #[test]
    fn a_tag_that_is_not_a_version_is_refused() {
        for reference in [
            "refs/tags/nightly",
            "v1.2",
            "v1.2.3.4",
            "v1.x.3",
            "v-heads/main",
        ] {
            assert_eq!(version_of(reference), None, "{reference}");
        }
    }

    #[test]
    fn a_crate_manifest_gets_the_new_version() {
        let text = "[package]\nname = \"cpp_parser\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";
        let rewritten = with_the_crate_version(text, "1.0.0").expect("a version line");
        assert!(rewritten.contains("version = \"1.0.0\""));
        assert_eq!(rewritten.matches("version =").count(), 1, "{rewritten}");
    }

    /// The workspace table is where the *published* version of a path dependency lives: without it `cargo publish`
    /// has no version to name, and the release build of a tag must not publish the committed one.
    #[test]
    fn the_workspace_table_gets_the_new_version() {
        let text = "[workspace.dependencies]\ncpp_parser = { path = \"crates/cpp_parser\", version = \"0.1.0\" }\nnot_cpp_ls = { path = \"crates/not_cpp_ls\" }\n";
        let mut out = String::new();
        for line in text.split_inclusive('\n') {
            match a_published_crate_on(line) {
                Some(name) => out.push_str(&format!(
                    "{name} = {{ path = \"crates/{name}\", version = \"1.0.0\" }}\n"
                )),
                None => out.push_str(line),
            }
        }
        assert!(out.contains("cpp_parser = { path = \"crates/cpp_parser\", version = \"1.0.0\" }"));
        assert!(
            out.contains("not_cpp_ls = { path = \"crates/not_cpp_ls\" }"),
            "{out}"
        );
    }
}
