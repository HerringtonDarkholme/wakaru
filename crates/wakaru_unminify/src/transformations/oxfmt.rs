use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::{parse_program, SourceFile};

pub fn transform_preserving_raw(source: &SourceFile) -> Result<String> {
    let allocator = Allocator::default();
    parse_program(&allocator, source)?;

    Ok(source.code.clone())
}

pub fn transform(source: &SourceFile) -> Result<String> {
    let allocator = Allocator::default();
    let ret = parse_program(&allocator, source)?;

    let options = CodegenOptions {
        indent_char: IndentChar::Space,
        indent_width: 2,
        ..CodegenOptions::default()
    };

    Ok(Codegen::new()
        .with_options(options)
        .build(&ret.program)
        .code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_inline_test;

    #[test]
    fn formats_basic_javascript() {
        let inline_test = define_inline_test(transform);

        inline_test(
            "
function foo(){return 1+2}
",
            "
function foo() {
  return 1 + 2;
}
",
        );
    }

    #[test]
    fn keeps_default_double_quotes() {
        let inline_test = define_inline_test(transform);

        inline_test(
            "
const foo='bar'
",
            r#"
const foo = "bar";
"#,
        );
    }
}
