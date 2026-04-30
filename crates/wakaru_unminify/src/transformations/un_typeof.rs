use oxc_allocator::TakeIn;
use oxc_ast::{ast::Expression, AstBuilder};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::{GetSpan, Span};
use oxc_syntax::operator::{BinaryOperator, UnaryOperator};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut normalizer = TypeofNormalizer {
        ast: AstBuilder::new(source.allocator),
    };

    normalizer.visit_program(&mut source.program);

    Ok(())
}

struct TypeofNormalizer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for TypeofNormalizer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        walk_mut::walk_expression(self, expression);

        if let Some(replacement) = self.typeof_undefined_replacement(expression) {
            *expression = replacement;
        }
    }
}

impl<'a> TypeofNormalizer<'a> {
    fn typeof_undefined_replacement(
        &mut self,
        expression: &mut Expression<'a>,
    ) -> Option<Expression<'a>> {
        let span = expression.span();
        let Expression::BinaryExpression(binary) = expression else {
            return None;
        };

        if is_typeof_expression(&binary.left) && is_u_string(&binary.right) {
            let operator = match binary.operator {
                BinaryOperator::LessThan => BinaryOperator::StrictInequality,
                BinaryOperator::GreaterThan => BinaryOperator::StrictEquality,
                _ => return None,
            };
            let string_span = binary.right.span();
            let typeof_expression = binary.left.take_in(self.ast);

            return Some(self.to_typeof_undefined(span, typeof_expression, operator, string_span));
        }

        if is_u_string(&binary.left) && is_typeof_expression(&binary.right) {
            let operator = match binary.operator {
                BinaryOperator::LessThan => BinaryOperator::StrictEquality,
                BinaryOperator::GreaterThan => BinaryOperator::StrictInequality,
                _ => return None,
            };
            let string_span = binary.left.span();
            let typeof_expression = binary.right.take_in(self.ast);

            return Some(self.to_typeof_undefined(span, typeof_expression, operator, string_span));
        }

        None
    }

    fn to_typeof_undefined(
        &self,
        span: Span,
        typeof_expression: Expression<'a>,
        operator: BinaryOperator,
        string_span: Span,
    ) -> Expression<'a> {
        let undefined = self
            .ast
            .expression_string_literal(string_span, "undefined", None);

        self.ast
            .expression_binary(span, typeof_expression, operator, undefined)
    }
}

fn is_typeof_expression(expression: &Expression) -> bool {
    matches!(
        expression,
        Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::Typeof
    )
}

fn is_u_string(expression: &Expression) -> bool {
    matches!(expression, Expression::StringLiteral(literal) if literal.value.as_str() == "u")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn transforms_minified_typeof_undefined() {
        define_ast_inline_test(transform_ast)(
            r#"
typeof x < "u";
"u" > typeof x;
typeof x > "u";
"u" < typeof x;
"#,
            r#"
typeof x !== "undefined";
typeof x !== "undefined";
typeof x === "undefined";
typeof x === "undefined";
"#,
        );
    }

    #[test]
    fn leaves_other_typeof_comparisons_unchanged() {
        define_ast_inline_test(transform_ast)(
            r#"
typeof x <= "u";
typeof x >= "u";
typeof x === "string";
typeof x === "number";
typeof x === "boolean";
typeof x === "symbol";
typeof x === "object";
typeof x === "bigint";
typeof x === "function";
typeof x === "undefined";
"#,
            r#"
typeof x <= "u";
typeof x >= "u";
typeof x === "string";
typeof x === "number";
typeof x === "boolean";
typeof x === "symbol";
typeof x === "object";
typeof x === "bigint";
typeof x === "function";
typeof x === "undefined";
"#,
        );
    }
}
