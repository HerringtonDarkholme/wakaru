use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{Expression, FunctionBody, Statement},
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_syntax::operator::UnaryOperator;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut simplifier = ReturnSimplifier {
        ast: AstBuilder::new(source.allocator),
    };

    simplifier.visit_program(&mut source.program);

    Ok(())
}

struct ReturnSimplifier<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for ReturnSimplifier<'a> {
    fn visit_function_body(&mut self, body: &mut FunctionBody<'a>) {
        walk_mut::walk_function_body(self, body);
        self.simplify_final_return(body);
    }
}

impl<'a> ReturnSimplifier<'a> {
    fn simplify_final_return(&self, body: &mut FunctionBody<'a>) {
        let Some(last_statement) = body.statements.last() else {
            return;
        };

        if !matches!(last_statement, Statement::ReturnStatement(_)) {
            return;
        }

        let mut statements = body.statements.take_in(self.ast);
        let Some(statement) = statements.pop() else {
            body.statements = statements;
            return;
        };

        let replacement = self.simplify_return_statement(statement);
        if let Some(statement) = replacement {
            statements.push(statement);
        }

        body.statements = statements;
    }

    fn simplify_return_statement(&self, statement: Statement<'a>) -> Option<Statement<'a>> {
        let Statement::ReturnStatement(mut return_statement) = statement else {
            return Some(statement);
        };

        let Some(argument) = &return_statement.argument else {
            return None;
        };

        if is_undefined(argument) {
            return None;
        }

        if is_void_expression(argument) {
            let span = return_statement.span;
            let Some(Expression::UnaryExpression(mut unary)) = return_statement.argument.take()
            else {
                return Some(Statement::ReturnStatement(return_statement));
            };

            let expression = unary.argument.take_in(self.ast);
            return Some(self.ast.statement_expression(span, expression));
        }

        Some(Statement::ReturnStatement(return_statement))
    }
}

fn is_undefined(expression: &Expression) -> bool {
    matches!(expression, Expression::Identifier(identifier) if identifier.name.as_str() == "undefined")
        || is_void_zero(expression)
}

fn is_void_zero(expression: &Expression) -> bool {
    let Expression::UnaryExpression(unary) = expression else {
        return false;
    };

    unary.operator == UnaryOperator::Void && is_numeric_zero(&unary.argument)
}

fn is_void_expression(expression: &Expression) -> bool {
    matches!(
        expression,
        Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::Void
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
    fn transforms_final_return_void_expression_to_expression_statement() {
        define_ast_inline_test(transform_ast)(
            "
function foo() {
  return void a()
}
",
            "
function foo() {
  a();
}
",
        );
    }

    #[test]
    fn removes_redundant_final_returns() {
        define_ast_inline_test(transform_ast)(
            "
function foo() {
  const a = 1
  return undefined
}

const bar = () => {
  const a = 1
  if (a) return void 0
  return void 0
}

const baz = function () {
  const a = 1
  if (a) {
    return undefined
  }
  return undefined
}

const obj = {
  method() {
    const a = 1
    return void 0
  }
}

class A {
  method() {
    const a = 1
    return
  }
}
",
            "
function foo() {
  const a = 1;
}
const bar = () => {
  const a = 1;
  if (a) return void 0;
};
const baz = function() {
  const a = 1;
  if (a) {
    return undefined;
  }
};
const obj = { method() {
  const a = 1;
} };
class A {
  method() {
    const a = 1;
  }
}
",
        );
    }

    #[test]
    fn only_simplifies_the_direct_final_function_return() {
        define_ast_inline_test(transform_ast)(
            "
function foo() {
  return void 0
  return undefined
}

function bar() {
  const count = 5;
  while (count--) {
    return void 0;
  }

  for (let i = 0; i < 10; i++) {
    return void foo();
  }
}
",
            "
function foo() {
  return void 0;
}
function bar() {
  const count = 5;
  while (count--) {
    return void 0;
  }
  for (let i = 0; i < 10; i++) {
    return void foo();
  }
}
",
        );
    }
}
