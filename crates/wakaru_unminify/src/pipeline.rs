use oxc_allocator::Allocator;
use oxc_ast::ast::Program;
use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
use wakaru_core::diagnostics::{Diagnostic, Result};
use wakaru_core::module::{ModuleMapping, ModuleMetaMap};
use wakaru_core::rules::TransformationDescriptor;
use wakaru_core::source::{parse_program, parse_source, ParsedSourceFile, SourceFile};
use wakaru_core::timing::Timing;

use crate::transformations::default_transformation_registry;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PipelineParams {
    pub module_mapping: ModuleMapping,
    pub module_meta: ModuleMetaMap,
}

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
    let _ = params;

    timing.measure(source.filename(), "oxc-parse", || parse_source(source))?;

    let mut current = source.clone();
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

            let code = run_ast_transformations(&current, &registry[start..index], &mut timing)?;
            current = SourceFile::from_parts(source.path.clone(), code);
        } else {
            let code = timing.measure(source.filename(), descriptor.id, || {
                descriptor.run_string(&current)
            })?;
            current = SourceFile::from_parts(source.path.clone(), code);
            index += 1;
        }
    }

    let code = current.code;
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
    timing: &mut Timing,
) -> Result<String> {
    let allocator = Allocator::default();
    let ret = parse_program(&allocator, source)?;
    let mut parsed_source = ParsedSourceFile::new(source, &allocator, ret.program);

    for descriptor in descriptors {
        timing.measure(source.filename(), descriptor.id, || {
            descriptor.run_ast(&mut parsed_source)
        })?;
    }

    Ok(generate_code(&parsed_source.program))
}

fn generate_code(program: &Program) -> String {
    let options = CodegenOptions {
        indent_char: IndentChar::Space,
        indent_width: 2,
        ..CodegenOptions::default()
    };

    Codegen::new().with_options(options).build(program).code
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

        assert_eq!(result.code, "exports.foo = 1;\n");
    }
}
