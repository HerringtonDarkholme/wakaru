use oxc_allocator::{CloneIn, TakeIn};
use oxc_ast::{
    ast::{AssignmentExpression, Expression, Statement},
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_syntax::operator::{AssignmentOperator, UnaryOperator};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut splitter = AssignmentMergingSplitter {
        ast: AstBuilder::new(source.allocator),
    };

    splitter.visit_program(&mut source.program);

    Ok(())
}

struct AssignmentMergingSplitter<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for AssignmentMergingSplitter<'a> {
    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        walk_mut::walk_statements(self, statements);

        let old_statements = statements.take_in(self.ast);
        let mut new_statements = self.ast.vec_with_capacity(old_statements.len());

        for statement in old_statements {
            if let Some(replacements) = self.split_assignment_statement(&statement) {
                for replacement in replacements {
                    new_statements.push(replacement);
                }
            } else {
                new_statements.push(statement);
            }
        }

        *statements = new_statements;
    }
}

impl<'a> AssignmentMergingSplitter<'a> {
    fn split_assignment_statement(
        &self,
        statement: &Statement<'a>,
    ) -> Option<oxc_allocator::Vec<'a, Statement<'a>>> {
        let Statement::ExpressionStatement(statement) = statement else {
            return None;
        };

        let Some((assignments, value)) = collect_assignment_chain(&statement.expression) else {
            return None;
        };

        if assignments.len() < 2 || !is_allowed_value(value) {
            return None;
        }

        let mut replacements = self.ast.vec_with_capacity(assignments.len());

        for assignment in assignments {
            let left = assignment.left.clone_in(self.ast.allocator);
            let right = value.clone_in(self.ast.allocator);
            let expression = self.ast.expression_assignment(
                assignment.span,
                AssignmentOperator::Assign,
                left,
                right,
            );

            replacements.push(self.ast.statement_expression(assignment.span, expression));
        }

        Some(replacements)
    }
}

fn collect_assignment_chain<'a, 'b>(
    expression: &'b Expression<'a>,
) -> Option<(
    std::vec::Vec<&'b AssignmentExpression<'a>>,
    &'b Expression<'a>,
)> {
    let Expression::AssignmentExpression(root) = expression else {
        return None;
    };

    if !root.operator.is_assign() {
        return None;
    }

    let mut assignments = std::vec::Vec::new();
    let mut current = root.as_ref();

    loop {
        assignments.push(current);

        let Expression::AssignmentExpression(next) = &current.right else {
            break;
        };

        if !next.operator.is_assign() {
            break;
        }

        current = next.as_ref();
    }

    Some((assignments, &current.right))
}

fn is_allowed_value(expression: &Expression) -> bool {
    matches!(
        expression,
        Expression::Identifier(_)
            | Expression::NullLiteral(_)
            | Expression::StringLiteral(_)
            | Expression::NumericLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::BigIntLiteral(_)
    ) || is_void_zero(expression)
        || is_loose_boolean(expression)
}

fn is_void_zero(expression: &Expression) -> bool {
    let Expression::UnaryExpression(unary) = expression else {
        return false;
    };

    unary.operator == UnaryOperator::Void && is_numeric_zero(&unary.argument)
}

fn is_loose_boolean(expression: &Expression) -> bool {
    matches!(expression, Expression::BooleanLiteral(_))
        || matches!(
            expression,
            Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::LogicalNot
        )
}

fn is_numeric_zero(expression: &Expression) -> bool {
    matches!(expression, Expression::NumericLiteral(literal) if literal.value == 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn splits_chained_assignments() {
        define_ast_inline_test(transform_ast)(
            "
exports.foo = exports.bar = exports.baz = 1;
",
            "
exports.foo = 1;
exports.bar = 1;
exports.baz = 1;
",
        );
    }

    #[test]
    fn splits_allowed_simple_values() {
        define_ast_inline_test(transform_ast)(
            "
a1 = a2 = 0;
b1 = b2 = 0n;
c1 = c2 = '';
d1 = d2 = true;
e1 = e2 = null;
f1 = f2 = undefined;
g1 = g2 = foo;
h1 = h2 = void 0;
i1 = i2 = !foo;
",
            "
a1 = 0;
a2 = 0;
b1 = 0n;
b2 = 0n;
c1 = \"\";
c2 = \"\";
d1 = true;
d2 = true;
e1 = null;
e2 = null;
f1 = undefined;
f2 = undefined;
g1 = foo;
g2 = foo;
h1 = void 0;
h2 = void 0;
i1 = !foo;
i2 = !foo;
",
        );
    }

    #[test]
    fn leaves_non_simple_values_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
a1 = a2 = `template${foo}`;
b1 = b2 = Symbol();
c1 = c2 = /regex/;
d1 = d2 = foo.bar;
f1 = f2 = fn();
",
            "
a1 = a2 = `template${foo}`;
b1 = b2 = Symbol();
c1 = c2 = /regex/;
d1 = d2 = foo.bar;
f1 = f2 = fn();
",
        );
    }
}
