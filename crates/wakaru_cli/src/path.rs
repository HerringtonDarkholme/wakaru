use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub fn resolve_file_globs(patterns: &[String]) -> Result<Vec<PathBuf>> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let mut files = Vec::new();

    for pattern in patterns {
        if pattern.trim().is_empty() {
            bail!("please enter a file path");
        }

        let path = Path::new(pattern);
        if path.exists() && path.is_dir() {
            bail!(
                "input is a directory; use a glob pattern instead, for example {}/**/*.js",
                pattern
            );
        }

        let normalized = pattern.replace('\\', "/");
        let walker = globwalk::GlobWalkerBuilder::from_patterns(&cwd, &[normalized.as_str()])
            .build()
            .with_context(|| format!("failed to resolve glob {pattern}"))?;

        for entry in walker.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if path.is_file() && !contains_node_modules(path) {
                files.push(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
            }
        }
    }

    files.sort();
    files.dedup();

    if files.is_empty() {
        bail!("no input files matched");
    }

    for file in &files {
        if !is_path_inside(&cwd, file) {
            bail!("input files must be inside the current working directory");
        }
    }

    Ok(files)
}

pub fn relative_path(from: &Path, to: &Path) -> String {
    let relative = to.strip_prefix(from).unwrap_or(to);
    let rendered = relative.display().to_string();
    if rendered.starts_with('.') {
        rendered
    } else {
        format!("./{rendered}")
    }
}

pub fn common_base_dir(paths: &[PathBuf]) -> Option<PathBuf> {
    let first = paths.first()?.canonicalize().ok()?;
    if paths.len() == 1 {
        return first.parent().map(Path::to_path_buf);
    }

    let mut common: Vec<_> = first.components().collect();

    for path in paths.iter().skip(1) {
        let path = path.canonicalize().ok()?;
        let parts: Vec<_> = path.components().collect();
        let len = common
            .iter()
            .zip(parts.iter())
            .take_while(|(a, b)| a == b)
            .count();
        common.truncate(len);
    }

    let mut base = PathBuf::new();
    for component in common {
        base.push(component.as_os_str());
    }

    if base.is_file() {
        base.pop();
    }

    Some(base)
}

pub fn ensure_output_available(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "output directory already exists at {}; pass --force to overwrite",
            path.display()
        );
    }

    Ok(())
}

fn is_path_inside(base: &Path, target: &Path) -> bool {
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    target.starts_with(base)
}

fn contains_node_modules(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "node_modules")
}
