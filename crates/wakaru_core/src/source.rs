use std::fs;
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::Program;
use oxc_parser::{Parser, ParserReturn};
use oxc_span::SourceType;

use crate::diagnostics::{Diagnostic, Result, WakaruError};
use crate::module::{ModuleMapping, ModuleMetaMap};

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

pub struct ParsedSourceFile<'a> {
    pub source: &'a SourceFile,
    pub allocator: &'a Allocator,
    pub program: Program<'a>,
    pub params: &'a TransformationParams,
    pub synthetic_trailing_comments: Vec<SyntheticTrailingComment>,
}

impl<'a> ParsedSourceFile<'a> {
    pub fn new(
        source: &'a SourceFile,
        allocator: &'a Allocator,
        program: Program<'a>,
        params: &'a TransformationParams,
    ) -> Self {
        Self {
            source,
            allocator,
            program,
            params,
            synthetic_trailing_comments: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TransformationParams {
    pub module_mapping: ModuleMapping,
    pub module_meta: ModuleMetaMap,
    pub un_esm_hoist: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntheticTrailingComment {
    pub candidates: Vec<String>,
    pub replacement: String,
}

pub fn parse_source(source: &SourceFile) -> Result<ParseSummary> {
    let allocator = Allocator::default();
    let ret = parse_program(&allocator, source)?;

    Ok(ParseSummary {
        statement_count: ret.program.body.len(),
        module_record_count: ret.module_record.requested_modules.len(),
    })
}

pub fn parse_program<'a>(
    allocator: &'a Allocator,
    source: &'a SourceFile,
) -> Result<ParserReturn<'a>> {
    let ret = Parser::new(allocator, &source.code, source_type_for_path(&source.path)).parse();

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

    Ok(ret)
}

pub fn source_type_for_path(path: &Path) -> SourceType {
    SourceType::from_path(path)
        .unwrap_or_default()
        .with_jsx(true)
}
