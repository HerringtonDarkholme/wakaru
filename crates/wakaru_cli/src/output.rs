use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use wakaru_core::module::{module_file_name, Module};

pub fn write_module(output_dir: &Path, module: &Module) -> Result<PathBuf> {
    let output_path = output_dir.join(module_file_name(module));
    write_file(&output_path, &module.code)?;
    Ok(output_path
        .canonicalize()
        .unwrap_or_else(|_| output_path.to_path_buf()))
}

pub fn write_file(path: &Path, code: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    fs::write(path, code).with_context(|| format!("failed to write {}", path.display()))
}
