use std::fs;
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;

use crate::diagnostics::{Diagnostic, Result, WakaruError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    pub path: PathBuf,
    pub code: String,
}

impl SourceFile {
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let code = fs::read_to_string(path)
            .map_err(|err| WakaruError::new(format!("failed to read {}: {err}", path.display())))?;

        Ok(Self {
            path: path.to_path_buf(),
            code,
        })
    }

    pub fn from_parts(path: impl Into<PathBuf>, code: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            code: code.into(),
        }
    }

    pub fn filename(&self) -> String {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseSummary {
    pub statement_count: usize,
    pub module_record_count: usize,
}

pub fn parse_source(source: &SourceFile) -> Result<ParseSummary> {
    let source_type = SourceType::from_path(&source.path)
        .unwrap_or_else(|_| SourceType::default().with_jsx(true));
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, &source.code, source_type).parse();

    if !ret.errors.is_empty() || ret.panicked {
        let diagnostics = ret
            .errors
            .into_iter()
            .map(|err| Diagnostic::error(format!("{err:?}")).with_path(source.path.clone()))
            .collect();

        return Err(WakaruError::with_diagnostics(
            format!("failed to parse {}", source.path.display()),
            diagnostics,
        ));
    }

    Ok(ParseSummary {
        statement_count: ret.program.body.len(),
        module_record_count: ret.module_record.requested_modules.len(),
    })
}
