use oxc_ast::ast::{NumericLiteral, UnaryExpression};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_syntax::{number::NumberBase, operator::UnaryOperator};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::{ParsedSourceFile, SyntheticTrailingComment};

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut normalizer = NumericLiteralNormalizer {
        synthetic_trailing_comments: &mut source.synthetic_trailing_comments,
        in_unary_negation: false,
    };

    normalizer.visit_program(&mut source.program);

    Ok(())
}

struct NumericLiteralNormalizer<'b> {
    synthetic_trailing_comments: &'b mut Vec<SyntheticTrailingComment>,
    in_unary_negation: bool,
}

impl<'a> VisitMut<'a> for NumericLiteralNormalizer<'_> {
    fn visit_unary_expression(&mut self, unary: &mut UnaryExpression<'a>) {
        let was_in_unary_negation = self.in_unary_negation;
        self.in_unary_negation = unary.operator == UnaryOperator::UnaryNegation;
        walk_mut::walk_unary_expression(self, unary);
        self.in_unary_negation = was_in_unary_negation;
    }

    fn visit_numeric_literal(&mut self, literal: &mut NumericLiteral<'a>) {
        self.normalize_literal(literal);
    }
}

impl NumericLiteralNormalizer<'_> {
    fn normalize_literal(&mut self, literal: &mut NumericLiteral) {
        let Some(raw) = literal.raw.as_ref().map(ToString::to_string) else {
            return;
        };

        let decimal = decimal_number_string(literal.value);
        if raw == decimal {
            return;
        }

        let raw_comment = if self.in_unary_negation {
            format!("-{raw}")
        } else {
            raw
        };

        self.synthetic_trailing_comments
            .push(SyntheticTrailingComment {
                candidates: rendered_number_candidates(&decimal, &raw_comment),
                replacement: format!("{decimal}/* {raw_comment} */"),
            });

        literal.raw = None;
        literal.base = if literal.value.fract() == 0.0 {
            NumberBase::Decimal
        } else {
            NumberBase::Float
        };
    }
}

fn rendered_number_candidates(decimal: &str, raw_comment: &str) -> Vec<String> {
    let raw = raw_comment.strip_prefix('-').unwrap_or(raw_comment);
    if raw == decimal {
        vec![decimal.to_string()]
    } else {
        vec![decimal.to_string(), raw.to_string()]
    }
}

fn decimal_number_string(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn transforms_number_literals_with_different_notation() {
        define_ast_inline_test(transform_ast)(
            "
65536;
123.4;
0b101010;
0o777;
-0x123;
4.2e2;
-2e4;
",
            "
65536;
123.4;
42/* 0b101010 */;
511/* 0o777 */;
-291/* -0x123 */;
420/* 4.2e2 */;
-20000/* -2e4 */;
",
        );
    }

    #[test]
    fn preserves_existing_leading_comment() {
        define_ast_inline_test(transform_ast)(
            "
// comment
0b101010;
",
            "
// comment
42/* 0b101010 */;
",
        );
    }
}
