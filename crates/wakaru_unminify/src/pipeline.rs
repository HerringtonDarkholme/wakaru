use oxc_allocator::Allocator;
use oxc_ast::ast::Program;
use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
use wakaru_core::diagnostics::{Diagnostic, Result};
use wakaru_core::rules::TransformationDescriptor;
use wakaru_core::source::{
    parse_program, parse_source, ParsedSourceFile, SourceFile, SyntheticTrailingComment,
    TransformationParams,
};
use wakaru_core::timing::Timing;

use crate::transformations::default_transformation_registry;

pub type PipelineParams = TransformationParams;

#[derive(Clone, Debug, PartialEq)]
pub struct TransformationResult {
    pub path: String,
    pub code: String,
    pub diagnostics: Vec<Diagnostic>,
    pub timing: Timing,
}

pub fn run_default_transformations(
    source: &SourceFile,
    params: PipelineParams,
) -> Result<TransformationResult> {
    let mut timing = Timing::default();
    timing.measure(source.filename(), "oxc-parse", || parse_source(source))?;

    let mut current = source.clone();
    let mut synthetic_trailing_comments = Vec::new();
    let registry = default_transformation_registry();
    let mut index = 0;

    while index < registry.len() {
        let descriptor = registry[index];

        if descriptor.kind.is_ast() {
            let start = index;
            index += 1;
            while index < registry.len() && registry[index].kind.is_ast() {
                index += 1;
            }

            let (code, comments) =
                run_ast_transformations(&current, &registry[start..index], &params, &mut timing)?;
            synthetic_trailing_comments.extend(comments);
            current = SourceFile::from_parts(source.path.clone(), code);
        } else {
            let code = timing.measure(source.filename(), descriptor.id, || {
                descriptor.run_string(&current)
            })?;
            current = SourceFile::from_parts(source.path.clone(), code);
            index += 1;
        }
    }

    let code = apply_synthetic_trailing_comments(current.code, &synthetic_trailing_comments);
    let transformed_source = SourceFile::from_parts(source.path.clone(), code.clone());
    timing.measure(source.filename(), "oxc-parse-output", || {
        parse_source(&transformed_source)
    })?;

    Ok(TransformationResult {
        path: source.path.display().to_string(),
        code,
        diagnostics: Vec::new(),
        timing,
    })
}

fn run_ast_transformations(
    source: &SourceFile,
    descriptors: &[TransformationDescriptor],
    params: &PipelineParams,
    timing: &mut Timing,
) -> Result<(String, Vec<SyntheticTrailingComment>)> {
    let allocator = Allocator::default();
    let ret = parse_program(&allocator, source)?;
    let mut parsed_source = ParsedSourceFile::new(source, &allocator, ret.program, params);

    for descriptor in descriptors {
        timing.measure(source.filename(), descriptor.id, || {
            descriptor.run_ast(&mut parsed_source)
        })?;
    }

    Ok((
        generate_code(&parsed_source.program),
        parsed_source.synthetic_trailing_comments,
    ))
}

fn generate_code(program: &Program) -> String {
    let options = CodegenOptions {
        indent_char: IndentChar::Space,
        indent_width: 2,
        ..CodegenOptions::default()
    };

    Codegen::new().with_options(options).build(program).code
}

fn apply_synthetic_trailing_comments(
    mut code: String,
    comments: &[SyntheticTrailingComment],
) -> String {
    let mut search_start = 0;

    for comment in comments {
        let Some((relative_index, candidate)) =
            find_first_candidate(&code[search_start..], comment)
        else {
            continue;
        };

        let start = search_start + relative_index;
        let end = start + candidate.len();
        code.replace_range(start..end, &comment.replacement);
        search_start = start + comment.replacement.len();
    }

    code
}

fn find_first_candidate<'a>(
    code: &str,
    comment: &'a SyntheticTrailingComment,
) -> Option<(usize, &'a str)> {
    comment
        .candidates
        .iter()
        .filter_map(|candidate| {
            code.find(candidate)
                .map(|index| (index, candidate.as_str()))
        })
        .min_by_key(|(index, _)| *index)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn formats_around_un_use_strict() {
        let source = SourceFile::from_parts(
            PathBuf::from("input.js"),
            "'use strict';function foo(){'use strict';return 1+2}",
        );

        let result = run_default_transformations(&source, PipelineParams::default())
            .expect("pipeline should succeed");

        assert_eq!(result.code, "function foo() {\n  return 1 + 2;\n}\n");
        assert_eq!(
            result
                .timing
                .stats()
                .iter()
                .map(|stat| stat.key.as_str())
                .collect::<Vec<_>>(),
            std::iter::once("oxc-parse")
                .chain(
                    default_transformation_registry()
                        .iter()
                        .map(|descriptor| descriptor.id)
                )
                .chain(std::iter::once("oxc-parse-output"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn removes_esmodule_flag() {
        let source = SourceFile::from_parts(
            PathBuf::from("input.js"),
            "Object.defineProperty(exports,'__esModule',{value:!0});exports.foo=1;",
        );

        let result = run_default_transformations(&source, PipelineParams::default())
            .expect("pipeline should succeed");

        assert_eq!(result.code, "export const foo = 1;\n");
    }

    #[test]
    fn preserves_numeric_literal_raw_comments_across_formatting_boundaries() {
        let source =
            SourceFile::from_parts(PathBuf::from("input.js"), "0b101010;-0x123;4.2e2;-2e4;");

        let result = run_default_transformations(&source, PipelineParams::default())
            .expect("pipeline should succeed");

        assert_eq!(
            result.code,
            "42/* 0b101010 */;\n-291/* -0x123 */;\n420/* 4.2e2 */;\n-20000/* -2e4 */;\n"
        );
    }
}
