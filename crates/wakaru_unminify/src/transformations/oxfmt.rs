use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
use oxc_parser::Parser;
use oxc_span::SourceType;
use wakaru_core::diagnostics::{Diagnostic, Result, WakaruError};
use wakaru_core::source::SourceFile;

pub fn transform(source: &SourceFile) -> Result<String> {
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
