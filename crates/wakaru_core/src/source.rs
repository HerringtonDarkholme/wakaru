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
            code: normalize_module_specifier_keywords(&code),
        })
    }

    pub fn from_parts(path: impl Into<PathBuf>, code: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            code: normalize_module_specifier_keywords(&code.into()),
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
    pub un_jsx_pragma: Option<String>,
    pub un_jsx_pragma_frag: Option<String>,
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

fn normalize_module_specifier_keywords(code: &str) -> String {
    let bytes = code.as_bytes();
    let mut output = String::new();
    let mut cursor = 0;
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                index = skip_quoted(bytes, index);
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index + 2);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index + 2);
            }
            _ if starts_with_keyword(bytes, index, b"import")
                || starts_with_keyword(bytes, index, b"export") =>
            {
                if let Some((start, end)) = module_specifier_list_span(bytes, index) {
                    let normalized = normalize_module_specifier_list(&code[start..end]);
                    if normalized != code[start..end] {
                        output.push_str(&code[cursor..start]);
                        output.push_str(&normalized);
                        cursor = end;
                    }
                    index = end;
                } else {
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }

    if cursor == 0 {
        return code.to_string();
    }

    output.push_str(&code[cursor..]);
    output
}

fn module_specifier_list_span(bytes: &[u8], keyword_start: usize) -> Option<(usize, usize)> {
    let mut index = keyword_start;
    while index < bytes.len() && bytes[index] != b'{' {
        if matches!(bytes[index], b';' | b'\n' | b'(' | b'.') {
            return None;
        }
        index += 1;
    }

    if bytes.get(index) != Some(&b'{') {
        return None;
    }

    let start = index + 1;
    let mut end = start;
    while end < bytes.len() {
        match bytes[end] {
            b'\'' | b'"' => end = skip_quoted(bytes, end),
            b'}' => return Some((start, end)),
            _ => end += 1,
        }
    }

    None
}

fn normalize_module_specifier_list(specifiers: &str) -> String {
    let bytes = specifiers.as_bytes();
    let mut output = String::new();
    let mut cursor = 0;
    let mut index = 0;

    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"') {
            index = skip_quoted(bytes, index);
            continue;
        }

        if !is_identifier_start(bytes[index]) {
            index += 1;
            continue;
        }

        let ident_start = index;
        index += 1;
        while index < bytes.len() && is_identifier_part(bytes[index]) {
            index += 1;
        }

        let ident = &specifiers[ident_start..index];
        if !is_reserved_module_specifier_keyword(ident) {
            continue;
        }

        let after_ident = skip_whitespace(bytes, index);
        if !starts_with_keyword(bytes, after_ident, b"as") {
            continue;
        }

        output.push_str(&specifiers[cursor..ident_start]);
        output.push('"');
        output.push_str(ident);
        output.push('"');
        cursor = index;
    }

    if cursor == 0 {
        return specifiers.to_string();
    }

    output.push_str(&specifiers[cursor..]);
    output
}

fn starts_with_keyword(bytes: &[u8], index: usize, keyword: &[u8]) -> bool {
    bytes
        .get(index..index + keyword.len())
        .is_some_and(|candidate| candidate == keyword)
        && index
            .checked_sub(1)
            .and_then(|previous| bytes.get(previous))
            .is_none_or(|byte| !is_identifier_part(*byte))
        && bytes
            .get(index + keyword.len())
            .is_none_or(|byte| !is_identifier_part(*byte))
}

fn skip_quoted(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            byte if byte == quote => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

fn skip_line_comment(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    index
}

fn skip_block_comment(bytes: &[u8], mut index: usize) -> usize {
    while index + 1 < bytes.len() {
        if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            return index + 2;
        }
        index += 1;
    }
    bytes.len()
}

fn skip_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte == b'$' || byte.is_ascii_alphabetic()
}

fn is_identifier_part(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

fn is_reserved_module_specifier_keyword(ident: &str) -> bool {
    matches!(
        ident,
        // Oxc 0.128 rejects these keyword module export names when they appear
        // as the imported side of `import { name as local }`.
        "do" | "in"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keyword_named_imports() {
        let source = SourceFile::from_parts(
            "fixture.js",
            r#"import { in as input, do as run, ok as ok } from "module";"#,
        );

        assert_eq!(
            source.code,
            r#"import { "in" as input, "do" as run, ok as ok } from "module";"#
        );
        parse_source(&source).expect("normalized keyword imports should parse");
    }

    #[test]
    fn leaves_import_like_strings_unchanged() {
        let source = SourceFile::from_parts(
            "fixture.js",
            r#"const text = "import { in as input } from 'module'";"#,
        );

        assert_eq!(
            source.code,
            r#"const text = "import { in as input } from 'module'";"#
        );
    }
}
