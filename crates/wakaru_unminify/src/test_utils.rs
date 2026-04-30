use std::path::PathBuf;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
use wakaru_core::diagnostics::Result;
use wakaru_core::rules::AstTransformationFn;
use wakaru_core::source::{parse_program, ParsedSourceFile, SourceFile};

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
        let mut parsed_source = ParsedSourceFile::new(&source, &allocator, ret.program);

        transform(&mut parsed_source).expect("transform should succeed");

        let output = Codegen::new()
            .with_options(CodegenOptions {
                indent_char: IndentChar::Space,
                indent_width: 2,
                ..CodegenOptions::default()
            })
            .build(&parsed_source.program)
            .code;

        assert_eq!(normalize(&output), normalize(expected));
    }
}

fn normalize(code: &str) -> String {
    code.trim().replace("\r\n", "\n")
}
