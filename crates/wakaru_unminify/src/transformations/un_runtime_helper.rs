use oxc_ast::ast::Statement;
use wakaru_core::diagnostics::Result;
use wakaru_core::module::ModuleMeta;
use wakaru_core::source::{ParsedSourceFile, SyntheticTrailingComment};

use crate::transformations::runtime_helpers;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    add_annotation_on_helper(source);
    runtime_helpers::transform_ast(source)
}

fn add_annotation_on_helper(source: &mut ParsedSourceFile) {
    let Some(module_meta) = current_module_meta(source) else {
        return;
    };

    let mut annotations = Vec::new();
    for statement in &source.program.body {
        let Statement::FunctionDeclaration(function) = statement else {
            continue;
        };
        let Some(function_id) = &function.id else {
            continue;
        };
        let Some(tags) = module_meta.tags.get(function_id.name.as_str()) else {
            continue;
        };
        if tags.is_empty() {
            continue;
        }

        annotations.push(SyntheticTrailingComment {
            candidates: vec![format!("function {}(", function_id.name.as_str())],
            replacement: format!(
                "{}\nfunction {}(",
                helper_annotation(tags),
                function_id.name.as_str()
            ),
        });
    }

    source.synthetic_trailing_comments.extend(annotations);
}

fn current_module_meta<'a>(source: &'a ParsedSourceFile<'_>) -> Option<&'a ModuleMeta> {
    let path = source.source.path.to_string_lossy();
    let filename = source.source.filename();
    let module_id = source
        .params
        .module_mapping
        .iter()
        .find_map(|(module_id, module_path)| {
            (module_path == path.as_ref() || module_path.as_str() == filename).then_some(module_id)
        })?;

    source.params.module_meta.get(module_id)
}

fn helper_annotation(tags: &[String]) -> String {
    let comment_content = tags
        .iter()
        .map(|tag| format!(" * {tag}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!("/**\n{comment_content}\n */")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use oxc_allocator::Allocator;
    use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
    use wakaru_core::module::{ModuleId, ModuleMapping, ModuleMeta, ModuleMetaMap};
    use wakaru_core::source::{parse_program, SourceFile, TransformationParams};

    use super::*;

    #[test]
    fn annotates_top_level_runtime_helper_functions_from_module_meta() {
        let source = SourceFile::from_parts(
            PathBuf::from("module-1.js"),
            "
function helper() {
  return 1;
}

function untouched() {
  return 2;
}
",
        );

        let output = transform_with_params(
            &source,
            TransformationParams {
                module_mapping: module_mapping([("1", "module-1.js")]),
                module_meta: module_meta([(
                    "1",
                    [(
                        "helper",
                        vec![
                            "@babel/runtime/helpers/extends",
                            "@babel/runtime/helpers/objectSpread2",
                        ],
                    )],
                )]),
                ..TransformationParams::default()
            },
        );

        assert_eq!(
            output.trim(),
            r#"/**
 * @babel/runtime/helpers/extends
 * @babel/runtime/helpers/objectSpread2
 */
function helper() {
  return 1;
}
function untouched() {
  return 2;
}"#
        );
    }

    #[test]
    fn skips_annotation_without_matching_module_meta() {
        let source = SourceFile::from_parts(
            PathBuf::from("module-2.js"),
            "
function helper() {
  return 1;
}
",
        );

        let output = transform_with_params(
            &source,
            TransformationParams {
                module_mapping: module_mapping([("1", "module-1.js")]),
                module_meta: module_meta([(
                    "1",
                    [("helper", vec!["@babel/runtime/helpers/extends"])],
                )]),
                ..TransformationParams::default()
            },
        );

        assert_eq!(output.trim(), "function helper() {\n  return 1;\n}");
    }

    fn transform_with_params(source: &SourceFile, params: TransformationParams) -> String {
        let allocator = Allocator::default();
        let ret = parse_program(&allocator, source).expect("input should parse");
        let mut parsed_source = ParsedSourceFile::new(source, &allocator, ret.program, &params);

        transform_ast(&mut parsed_source).expect("transform should succeed");

        let output = Codegen::new()
            .with_options(CodegenOptions {
                indent_char: IndentChar::Space,
                indent_width: 2,
                ..CodegenOptions::default()
            })
            .build(&parsed_source.program)
            .code;

        apply_synthetic_trailing_comments(output, &parsed_source.synthetic_trailing_comments)
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

    fn module_mapping<const N: usize>(entries: [(&str, &str); N]) -> ModuleMapping {
        entries
            .into_iter()
            .map(|(id, path)| (ModuleId::new(id), path.to_string()))
            .collect()
    }

    fn module_meta<const N: usize, const M: usize>(
        entries: [(&str, [(&str, Vec<&str>); M]); N],
    ) -> ModuleMetaMap {
        entries
            .into_iter()
            .map(|(id, tags)| {
                (
                    ModuleId::new(id),
                    ModuleMeta {
                        imports: Vec::new(),
                        exports: BTreeMap::new(),
                        tags: tags
                            .into_iter()
                            .map(|(name, tags)| {
                                (
                                    name.to_string(),
                                    tags.into_iter().map(str::to_string).collect(),
                                )
                            })
                            .collect(),
                    },
                )
            })
            .collect()
    }
}
