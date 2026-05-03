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
    fn visit_statement(&mut self, statement: &mut Statement<'a>) {
        walk_mut::walk_statement(self, statement);

        let Statement::IfStatement(if_statement) = statement else {
            return;
        };

        self.render_if_branch(&mut if_statement.consequent);
        if let Some(alternate) = &mut if_statement.alternate {
            self.render_if_branch(alternate);
        }
    }

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
    fn render_if_branch(&self, branch: &mut Statement<'a>) {
        let Statement::ExpressionStatement(expression_statement) = branch else {
            return;
        };

        let Some(statements) = self.render_expression(&expression_statement.expression) else {
            return;
        };

        *branch = self.block_statement_from_body(expression_statement.span, statements);
    }

    fn render_statement(&self, statement: Statement<'a>) -> oxc_allocator::Vec<'a, Statement<'a>> {
        match statement {
            Statement::ExpressionStatement(expression_statement) => {
                self.render_expression_statement(expression_statement.unbox())
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
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let span = expression_statement.span;
        if let Some(statements) = self.render_expression(&expression_statement.expression) {
            return statements;
        }

        self.single(
            self.ast
                .statement_expression(span, expression_statement.expression),
        )
    }

    fn render_expression(
        &self,
        expression: &Expression<'a>,
    ) -> Option<oxc_allocator::Vec<'a, Statement<'a>>> {
        match without_parentheses(expression) {
            Expression::ConditionalExpression(conditional) => {
                self.render_conditional_expression(conditional)
            }
            Expression::LogicalExpression(logical) => self.render_logical_expression(logical),
            expression if should_render_leaf(expression) => {
                Some(self.single(self.ast.statement_expression(
                    expression.span(),
                    expression.clone_in(self.ast.allocator),
                )))
            }
            _ => None,
        }
    }

    fn render_conditional_expression(
        &self,
        conditional: &ConditionalExpression<'a>,
    ) -> Option<oxc_allocator::Vec<'a, Statement<'a>>> {
        let true_branch = self.render_expression(&conditional.consequent)?;
        let mut false_branch = self.render_expression(&conditional.alternate)?;

        let alternate = if false_branch.len() == 1
            && matches!(false_branch.first(), Some(Statement::IfStatement(_)))
        {
            false_branch.pop()
        } else {
            Some(self.block_statement_from_body(conditional.alternate.span(), false_branch))
        };

        Some(self.single(self.ast.statement_if(
            conditional.span,
            conditional.test.clone_in(self.ast.allocator),
            self.block_statement_from_body(conditional.consequent.span(), true_branch),
            alternate,
        )))
    }

    fn render_logical_expression(
        &self,
        logical: &LogicalExpression<'a>,
    ) -> Option<oxc_allocator::Vec<'a, Statement<'a>>> {
        let test = match logical.operator {
            LogicalOperator::And => logical.left.clone_in(self.ast.allocator),
            LogicalOperator::Or => self.negate_condition(&logical.left),
            LogicalOperator::Coalesce => return None,
        };
        let body = self.render_expression(&logical.right)?;

        Some(self.single(self.ast.statement_if(
            logical.span,
            test,
            self.block_statement_from_body(logical.right.span(), body),
            None,
        )))
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
        without_parentheses(expression),
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

fn without_parentheses<'a, 'b>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            without_parentheses(&parenthesized.expression)
        }
        _ => expression,
    }
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
    fn renders_nested_ternary_expression_statements() {
        define_ast_inline_test(transform_ast)(
            "
a ? b() : c ? d() : e() ? g ? h() : i() : j();
foo ? x() : bar ? y() : baz && z();
foo ? x() : bar ? y() : baz ? z() : t();
",
            "
if (a) {
  b();
} else if (c) {
  d();
} else if (e()) {
  if (g) {
    h();
  } else {
    i();
  }
} else {
  j();
}
if (foo) {
  x();
} else if (bar) {
  y();
} else if (baz) {
  z();
}
if (foo) {
  x();
} else if (bar) {
  y();
} else if (baz) {
  z();
} else {
  t();
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
    fn renders_nested_logical_expression_statements() {
        define_ast_inline_test(transform_ast)(
            "
a ? b() : c ? d() : e() && (g || h());
x == 'a' || x == 'b' || x == 'c' && finished();
",
            "
if (a) {
  b();
} else if (c) {
  d();
} else if (e()) {
  if (!g) {
    h();
  }
}
if (!(x == \"a\" || x == \"b\")) {
  if (x == \"c\") {
    finished();
  }
}
",
        );
    }

    #[test]
    fn renders_logical_expression_in_if_branches() {
        define_ast_inline_test(transform_ast)(
            "
if (x) null === state && a();
else if (y) null !== state && b();
",
            "
if (x) {
  if (null === state) {
    a();
  }
} else if (y) {
  if (null !== state) {
    b();
  }
}
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
