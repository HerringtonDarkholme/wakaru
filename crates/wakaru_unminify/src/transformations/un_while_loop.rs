use oxc_allocator::TakeIn;
use oxc_ast::{ast::Statement, AstBuilder};
use oxc_ast_visit::{walk_mut, VisitMut};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut normalizer = WhileLoopNormalizer {
        ast: AstBuilder::new(source.allocator),
    };

    normalizer.visit_program(&mut source.program);

    Ok(())
}

struct WhileLoopNormalizer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for WhileLoopNormalizer<'a> {
    fn visit_statement(&mut self, statement: &mut Statement<'a>) {
        walk_mut::walk_statement(self, statement);
        self.normalize_for_statement(statement);
    }
}

impl<'a> WhileLoopNormalizer<'a> {
    fn normalize_for_statement(&self, statement: &mut Statement<'a>) {
        let Statement::ForStatement(for_statement) = statement else {
            return;
        };

        if for_statement.init.is_some() || for_statement.update.is_some() {
            return;
        }

        let span = for_statement.span;
        let test = for_statement
            .test
            .take()
            .unwrap_or_else(|| self.ast.expression_boolean_literal(span, true));
        let body = for_statement.body.take_in(self.ast);

        *statement = Statement::WhileStatement(self.ast.alloc_while_statement(span, test, body));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn transforms_for_without_init_or_update_to_while() {
        define_ast_inline_test(transform_ast)(
            "
for (;;) {
  console.log('hello')
}

for (; i < 10;) {
  console.log('hello')
}
",
            "
while (true) {
  console.log(\"hello\");
}
while (i < 10) {
  console.log(\"hello\");
}
",
        );
    }

    #[test]
    fn leaves_for_with_init_or_update_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
for (let i = 0;;) {}

for (;; i++) {}
",
            "
for (let i = 0;;) {}
for (;; i++) {}
",
        );
    }

    #[test]
    fn transforms_nested_eligible_for_loops() {
        define_ast_inline_test(transform_ast)(
            "
for (;;) {
  for (; ok;) {
    run()
  }
}
",
            "
while (true) {
  while (ok) {
    run();
  }
}
",
        );
    }
}
