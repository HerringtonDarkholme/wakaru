use oxc_ast::{ast::Expression, AstBuilder};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::GetSpan;
use oxc_syntax::operator::{BinaryOperator, UnaryOperator};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut normalizer = InfinityNormalizer {
        ast: AstBuilder::new(source.allocator),
    };

    normalizer.visit_program(&mut source.program);

    Ok(())
}

struct InfinityNormalizer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for InfinityNormalizer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        if let Some(sign) = minified_infinity_sign(expression) {
            let span = expression.span();
            let infinity = self.ast.expression_identifier(span, "Infinity");
            *expression = match sign {
                InfinitySign::Positive => infinity,
                InfinitySign::Negative => {
                    self.ast
                        .expression_unary(span, UnaryOperator::UnaryNegation, infinity)
                }
            };
            return;
        }

        walk_mut::walk_expression(self, expression);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InfinitySign {
    Positive,
    Negative,
}

fn minified_infinity_sign(expression: &Expression) -> Option<InfinitySign> {
    let Expression::BinaryExpression(binary) = expression else {
        return None;
    };

    if binary.operator != BinaryOperator::Division || !is_numeric_literal(&binary.right, 0.0) {
        return None;
    }

    if is_numeric_literal(&binary.left, 1.0) {
        return Some(InfinitySign::Positive);
    }

    if is_negative_one(&binary.left) {
        return Some(InfinitySign::Negative);
    }

    None
}

fn is_negative_one(expression: &Expression) -> bool {
    let Expression::UnaryExpression(unary) = expression else {
        return false;
    };

    unary.operator == UnaryOperator::UnaryNegation && is_numeric_literal(&unary.argument, 1.0)
}

fn is_numeric_literal(expression: &Expression, expected: f64) -> bool {
    matches!(expression, Expression::NumericLiteral(literal) if literal.value == expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn transforms_one_divided_by_zero_to_infinity() {
        define_ast_inline_test(transform_ast)(
            "
0 / 0;
1 / 0;
-1 / 0;
99 / 0;

'0' / 0;
'1' / 0;
'-1' / 0;
'99' / 0;

x / 0;

[0 / 0, 1 / 0]
",
            "
0 / 0;
Infinity;
-Infinity;
99 / 0;
\"0\" / 0;
\"1\" / 0;
\"-1\" / 0;
\"99\" / 0;
x / 0;
[0 / 0, Infinity];
",
        );
    }

    #[test]
    fn leaves_similar_divisions_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
1 / 1;
2 / 0;
1 / foo;
(-1) / 1;
",
            "
1 / 1;
2 / 0;
1 / foo;
-1 / 1;
",
        );
    }
}
