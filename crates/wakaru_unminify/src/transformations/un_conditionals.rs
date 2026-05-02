use oxc_allocator::{CloneIn, TakeIn};
use oxc_ast::{
    ast::{Expression, LogicalExpression, Statement},
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::{GetSpan, Span};
use oxc_syntax::operator::{LogicalOperator, UnaryOperator};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut renderer = ConditionalRenderer {
        ast: AstBuilder::new(source.allocator),
    };

    renderer.visit_program(&mut source.program);

    Ok(())
}

struct ConditionalRenderer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for ConditionalRenderer<'a> {
    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        walk_mut::walk_statements(self, statements);

        let old_statements = statements.take_in(self.ast);
        let mut new_statements = self.ast.vec_with_capacity(old_statements.len());

        for statement in old_statements {
            new_statements.push(self.render_statement(statement));
        }

        *statements = new_statements;
    }
}

impl<'a> ConditionalRenderer<'a> {
    fn render_statement(&self, statement: Statement<'a>) -> Statement<'a> {
        let Statement::ExpressionStatement(expression_statement) = statement else {
            return statement;
        };

        let span = expression_statement.span;
        match &expression_statement.expression {
            Expression::ConditionalExpression(conditional)
                if should_render_leaf(&conditional.consequent)
                    && should_render_leaf(&conditional.alternate) =>
            {
                let consequent = self.block_statement(conditional.consequent.span(), |body| {
                    body.push(self.ast.statement_expression(
                        conditional.consequent.span(),
                        conditional.consequent.clone_in(self.ast.allocator),
                    ));
                });
                let alternate = self.block_statement(conditional.alternate.span(), |body| {
                    body.push(self.ast.statement_expression(
                        conditional.alternate.span(),
                        conditional.alternate.clone_in(self.ast.allocator),
                    ));
                });
                self.ast.statement_if(
                    span,
                    conditional.test.clone_in(self.ast.allocator),
                    consequent,
                    Some(alternate),
                )
            }
            Expression::LogicalExpression(logical) => self
                .render_logical_expression_statement(span, logical)
                .unwrap_or(Statement::ExpressionStatement(expression_statement)),
            _ => Statement::ExpressionStatement(expression_statement),
        }
    }

    fn render_logical_expression_statement(
        &self,
        span: Span,
        logical: &LogicalExpression<'a>,
    ) -> Option<Statement<'a>> {
        if !should_render_leaf(&logical.right) {
            return None;
        }

        let test = match logical.operator {
            LogicalOperator::And => logical.left.clone_in(self.ast.allocator),
            LogicalOperator::Or => self.negate_condition(&logical.left),
            LogicalOperator::Coalesce => return None,
        };
        let consequent = self.block_statement(logical.right.span(), |body| {
            body.push(self.ast.statement_expression(
                logical.right.span(),
                logical.right.clone_in(self.ast.allocator),
            ));
        });

        Some(self.ast.statement_if(span, test, consequent, None))
    }

    fn negate_condition(&self, expression: &Expression<'a>) -> Expression<'a> {
        match expression {
            Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::LogicalNot => {
                unary.argument.clone_in(self.ast.allocator)
            }
            expression => self.ast.expression_unary(
                expression.span(),
                UnaryOperator::LogicalNot,
                expression.clone_in(self.ast.allocator),
            ),
        }
    }

    fn block_statement(
        &self,
        span: Span,
        build: impl FnOnce(&mut oxc_allocator::Vec<'a, Statement<'a>>),
    ) -> Statement<'a> {
        let mut body = self.ast.vec();
        build(&mut body);
        self.ast.statement_block(span, body)
    }
}

fn should_render_leaf(expression: &Expression) -> bool {
    !matches!(
        expression,
        Expression::Identifier(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::NumericLiteral(_)
            | Expression::BigIntLiteral(_)
            | Expression::RegExpLiteral(_)
            | Expression::StringLiteral(_)
            | Expression::TemplateLiteral(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn renders_simple_ternary_expression_statement() {
        define_ast_inline_test(transform_ast)(
            "
x ? a() : b();
",
            "
if (x) {
  a();
} else {
  b();
}
",
        );
    }

    #[test]
    fn renders_simple_logical_expression_statements() {
        define_ast_inline_test(transform_ast)(
            "
x && a();
x || b();
!x && a();
!x || b();
x ?? c();
",
            "
if (x) {
  a();
}
if (!x) {
  b();
}
if (!x) {
  a();
}
if (x) {
  b();
}
x ?? c();
",
        );
    }

    #[test]
    fn leaves_value_leaf_conditionals_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
x ? a : b;
x ? 1 : 2;
x && 1;
",
            "
x ? a : b;
x ? 1 : 2;
x && 1;
",
        );
    }
}
