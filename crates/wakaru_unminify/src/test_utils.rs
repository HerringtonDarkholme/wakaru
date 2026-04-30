use std::path::PathBuf;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
use wakaru_core::diagnostics::Result;
use wakaru_core::rules::AstTransformationFn;
use wakaru_core::source::{
    parse_program, ParsedSourceFile, SourceFile, SyntheticTrailingComment, TransformationParams,
};

pub(crate) fn define_inline_test(
    transform: fn(&SourceFile) -> Result<String>,
) -> impl Fn(&str, &str) {
    move |input, expected| {
        let source = SourceFile::from_parts(PathBuf::from("test.js"), input);
        let output = transform(&source).expect("transform should succeed");

        assert_eq!(normalize(&output), normalize(expected));
    }
}

pub(crate) fn define_ast_inline_test(transform: AstTransformationFn) -> impl Fn(&str, &str) {
    move |input, expected| {
        let source = SourceFile::from_parts(PathBuf::from("test.js"), input);
        let allocator = Allocator::default();
        let ret = parse_program(&allocator, &source).expect("input should parse");
        let params = TransformationParams::default();
        let mut parsed_source = ParsedSourceFile::new(&source, &allocator, ret.program, &params);

        transform(&mut parsed_source).expect("transform should succeed");

        let output = Codegen::new()
            .with_options(CodegenOptions {
                indent_char: IndentChar::Space,
                indent_width: 2,
                ..CodegenOptions::default()
            })
            .build(&parsed_source.program)
            .code;
        let output =
            apply_synthetic_trailing_comments(output, &parsed_source.synthetic_trailing_comments);

        assert_eq!(normalize(&output), normalize(expected));
    }
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

fn normalize(code: &str) -> String {
    code.trim().replace("\r\n", "\n")
}
