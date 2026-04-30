use oxc_allocator::Allocator;
use oxc_ast::ast::Directive;
use oxc_ast_visit::{walk, Visit};
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};
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

    let mut collector = UseStrictCollector::default();
    collector.visit_program(&ret.program);

    Ok(remove_directives(&source.code, &collector.spans))
}

#[derive(Default)]
struct UseStrictCollector {
    spans: Vec<Span>,
}

impl<'a> Visit<'a> for UseStrictCollector {
    fn visit_directive(&mut self, directive: &Directive<'a>) {
        if directive.directive.as_str() == "use strict" {
            self.spans.push(directive.span);
        }

        walk::walk_directive(self, directive);
    }
}

fn remove_directives(source: &str, spans: &[Span]) -> String {
    if spans.is_empty() {
        return source.to_string();
    }

    let mut ranges = spans
        .iter()
        .map(|span| removal_range(source, span))
        .collect::<Vec<_>>();
    ranges.sort_unstable_by_key(|(start, _)| *start);

    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;

    for (start, end) in ranges {
        if start < cursor {
            continue;
        }

        output.push_str(&source[cursor..start]);
        cursor = end;
    }

    output.push_str(&source[cursor..]);
    output
}

fn removal_range(source: &str, span: &Span) -> (usize, usize) {
    let mut start = span.start as usize;
    let mut end = span.end as usize;
    let bytes = source.as_bytes();

    while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
        start -= 1;
    }

    while end < bytes.len() && matches!(bytes[end], b' ' | b'\t') {
        end += 1;
    }

    if end < bytes.len() && bytes[end] == b';' {
        end += 1;
        while end < bytes.len() && matches!(bytes[end], b' ' | b'\t') {
            end += 1;
        }
    }

    if end + 1 < bytes.len() && bytes[end] == b'\r' && bytes[end + 1] == b'\n' {
        end += 2;
    } else if end < bytes.len() && matches!(bytes[end], b'\n' | b'\r') {
        end += 1;
    }

    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_inline_test;

    #[test]
    fn removes_top_level_use_strict() {
        let inline_test = define_inline_test(transform);

        inline_test(
            "
'use strict'
",
            "
",
        );
    }

    #[test]
    fn removes_use_strict_with_comments() {
        let inline_test = define_inline_test(transform);

        inline_test(
            "
// comment
// another comment
'use strict'
function foo(str) {
  'use strict'
  return str === 'use strict'
}
",
            "
// comment
// another comment
function foo(str) {
  return str === 'use strict'
}
",
        );
    }

    #[test]
    fn leaves_non_directive_string_literals() {
        let inline_test = define_inline_test(transform);

        inline_test(
            "
function foo(str) {
  return str === 'use strict'
}
",
            "
function foo(str) {
  return str === 'use strict'
}
",
        );
    }
}
