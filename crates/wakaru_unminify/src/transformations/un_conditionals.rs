use oxc_allocator::{CloneIn, TakeIn};
use oxc_ast::{
    ast::{ConditionalExpression, Expression, ExpressionStatement, LogicalExpression, Statement},
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
            for replacement in self.render_statement(statement) {
                new_statements.push(replacement);
            }
        }

        *statements = new_statements;
    }
}

impl<'a> ConditionalRenderer<'a> {
    fn render_statement(&self, statement: Statement<'a>) -> oxc_allocator::Vec<'a, Statement<'a>> {
        match statement {
            Statement::ExpressionStatement(expression_statement) => {
                self.single(self.render_expression_statement(expression_statement.unbox()))
            }
            Statement::ReturnStatement(return_statement) => {
                let Some(Expression::ConditionalExpression(conditional)) =
                    &return_statement.argument
                else {
                    return self.single(Statement::ReturnStatement(return_statement));
                };

                if should_render_return_conditional(conditional) {
                    self.render_return_conditional(conditional)
                } else {
                    self.single(Statement::ReturnStatement(return_statement))
                }
            }
            statement => self.single(statement),
        }
    }

    fn render_expression_statement(
        &self,
        expression_statement: ExpressionStatement<'a>,
    ) -> Statement<'a> {
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
            Expression::LogicalExpression(logical) => {
                if let Some(statement) = self.render_logical_expression_statement(span, logical) {
                    statement
                } else {
                    self.ast
                        .statement_expression(span, expression_statement.expression)
                }
            }
            _ => self
                .ast
                .statement_expression(span, expression_statement.expression),
        }
    }

    fn render_return_conditional(
        &self,
        conditional: &ConditionalExpression<'a>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let true_branch = self.render_return_expression(&conditional.consequent);
        let false_branch = self.render_return_expression(&conditional.alternate);

        let mut statements = self.ast.vec_with_capacity(1 + false_branch.len());
        statements.push(self.ast.statement_if(
            conditional.span,
            conditional.test.clone_in(self.ast.allocator),
            self.block_statement_from_body(conditional.consequent.span(), true_branch),
            None,
        ));

        for statement in false_branch {
            statements.push(statement);
        }

        statements
    }

    fn render_return_expression(
        &self,
        expression: &Expression<'a>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        if let Expression::ConditionalExpression(conditional) = expression {
            return self.render_return_conditional(conditional);
        };

        self.single(self.ast.statement_return(
            expression.span(),
            Some(expression.clone_in(self.ast.allocator)),
        ))
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

    fn block_statement_from_body(
        &self,
        span: Span,
        body: oxc_allocator::Vec<'a, Statement<'a>>,
    ) -> Statement<'a> {
        self.ast.statement_block(span, body)
    }

    fn single(&self, statement: Statement<'a>) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let mut statements = self.ast.vec_with_capacity(1);
        statements.push(statement);
        statements
    }
}

fn should_render_return_conditional(conditional: &ConditionalExpression) -> bool {
    has_conditional_branch(conditional)
        && should_render_return_expression(&conditional.consequent)
        && should_render_return_expression(&conditional.alternate)
}

fn should_render_return_expression(expression: &Expression) -> bool {
    match expression {
        Expression::ConditionalExpression(conditional) => {
            should_render_return_expression(&conditional.consequent)
                && should_render_return_expression(&conditional.alternate)
        }
        expression => should_render_leaf(expression),
    }
}

fn has_conditional_branch(conditional: &ConditionalExpression) -> bool {
    matches!(
        &conditional.consequent,
        Expression::ConditionalExpression(_)
    ) || matches!(&conditional.alternate, Expression::ConditionalExpression(_))
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

    #[test]
    fn renders_nested_ternary_return_as_early_returns() {
        define_ast_inline_test(transform_ast)(
            "
function fn() {
  return a ? b() : c ? d() : e();
}
",
            "
function fn() {
  if (a) {
    return b();
  }
  if (c) {
    return d();
  }
  return e();
}
",
        );
    }

    #[test]
    fn leaves_simple_ternary_return_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
function fn() {
  return x ? a() : b();
}
",
            "
function fn() {
  return x ? a() : b();
}
",
        );
    }
}
