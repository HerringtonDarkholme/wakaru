use oxc_ast::ast::Directive;
use oxc_ast_visit::{walk_mut, VisitMut};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    UseStrictRemover.visit_program(&mut source.program);
    Ok(())
}

struct UseStrictRemover;

impl<'a> VisitMut<'a> for UseStrictRemover {
    fn visit_directives(&mut self, directives: &mut oxc_allocator::Vec<'a, Directive<'a>>) {
        directives.retain(|directive| directive.directive.as_str() != "use strict");
        walk_mut::walk_directives(self, directives);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn removes_top_level_use_strict() {
        let inline_test = define_ast_inline_test(transform_ast);

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
        let inline_test = define_ast_inline_test(transform_ast);

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
function foo(str) {
  return str === \"use strict\";
}
",
        );
    }

    #[test]
    fn leaves_non_directive_string_literals() {
        let inline_test = define_ast_inline_test(transform_ast);

        inline_test(
            "
function foo(str) {
  return str === \"use strict\";
}
",
            "
function foo(str) {
  return str === \"use strict\";
}
",
        );
    }
}
