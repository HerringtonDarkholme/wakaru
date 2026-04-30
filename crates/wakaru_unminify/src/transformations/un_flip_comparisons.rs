use oxc_ast::ast::{BinaryExpression, Expression};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_syntax::operator::{BinaryOperator, UnaryOperator};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut flipper = ComparisonFlipper;

    flipper.visit_program(&mut source.program);

    Ok(())
}

struct ComparisonFlipper;

impl<'a> VisitMut<'a> for ComparisonFlipper {
    fn visit_binary_expression(&mut self, binary: &mut BinaryExpression<'a>) {
        walk_mut::walk_binary_expression(self, binary);

        if !is_comparison_operator(binary.operator)
            || !is_right_valid(&binary.right)
            || !is_left_valid(&binary.left)
        {
            return;
        }

        std::mem::swap(&mut binary.left, &mut binary.right);

        if let Some(flipped) = flipped_relational_operator(binary.operator) {
            binary.operator = flipped;
        }
    }
}

fn is_comparison_operator(operator: BinaryOperator) -> bool {
    matches!(
        operator,
        BinaryOperator::Equality
            | BinaryOperator::StrictEquality
            | BinaryOperator::Inequality
            | BinaryOperator::StrictInequality
            | BinaryOperator::LessThan
            | BinaryOperator::GreaterThan
            | BinaryOperator::LessEqualThan
            | BinaryOperator::GreaterEqualThan
    )
}

fn flipped_relational_operator(operator: BinaryOperator) -> Option<BinaryOperator> {
    match operator {
        BinaryOperator::LessThan => Some(BinaryOperator::GreaterThan),
        BinaryOperator::GreaterThan => Some(BinaryOperator::LessThan),
        BinaryOperator::LessEqualThan => Some(BinaryOperator::GreaterEqualThan),
        BinaryOperator::GreaterEqualThan => Some(BinaryOperator::LessEqualThan),
        _ => None,
    }
}

fn is_left_valid(expression: &Expression) -> bool {
    if is_void_zero(expression) {
        return true;
    }

    match expression {
        Expression::NullLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NumericLiteral(_)
        | Expression::StringLiteral(_) => true,
        Expression::Identifier(identifier) => is_common_value_identifier(identifier.name.as_str()),
        Expression::UnaryExpression(unary) => {
            matches!(&unary.argument, Expression::Identifier(identifier) if is_common_value_identifier(identifier.name.as_str()))
        }
        Expression::TemplateLiteral(template) => template.expressions.is_empty(),
        _ => false,
    }
}

fn is_right_valid(expression: &Expression) -> bool {
    let expression = match expression {
        Expression::UnaryExpression(unary) => &unary.argument,
        _ => expression,
    };

    matches!(
        expression,
        Expression::Identifier(_)
            | Expression::StaticMemberExpression(_)
            | Expression::ComputedMemberExpression(_)
            | Expression::PrivateFieldExpression(_)
            | Expression::CallExpression(_)
    )
}

fn is_void_zero(expression: &Expression) -> bool {
    let Expression::UnaryExpression(unary) = expression else {
        return false;
    };

    unary.operator == UnaryOperator::Void
        && matches!(&unary.argument, Expression::NumericLiteral(literal) if literal.value == 0.0)
}

fn is_common_value_identifier(name: &str) -> bool {
    matches!(name, "undefined" | "NaN" | "Infinity")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn flips_comparisons() {
        define_ast_inline_test(transform_ast)(
            r#"
void 0 === foo;
undefined === foo;
null !== foo;
1 == foo;
true != foo;
"str" == foo;
`test` == foo;
NaN == foo;
Infinity == foo;
-Infinity == foo;
"function" == typeof foo;

1 < bar;
1 > bar;
1 <= bar;
1 >= bar;
"#,
            r#"
foo === void 0;
foo === undefined;
foo !== null;
foo == 1;
foo != true;
foo == "str";
foo == `test`;
foo == NaN;
foo == Infinity;
foo == -Infinity;
typeof foo == "function";
bar > 1;
bar < 1;
bar >= 1;
bar <= 1;
"#,
        );
    }

    #[test]
    fn flips_comparisons_for_various_right_hand_values() {
        define_ast_inline_test(transform_ast)(
            r#"
1 == obj.props;
1 == obj.props[0];
1 == method();
"#,
            r#"
obj.props == 1;
obj.props[0] == 1;
method() == 1;
"#,
        );
    }

    #[test]
    fn flips_comparison_on_conditional_expression() {
        define_ast_inline_test(transform_ast)(
            r#"
2 === foo ? bar : baz;
"#,
            r#"
foo === 2 ? bar : baz;
"#,
        );
    }

    #[test]
    fn leaves_non_matching_comparisons() {
        define_ast_inline_test(transform_ast)(
            r#"
foo === undefined;
foo !== null;
foo == 1;
foo != true;
foo == "str";
foo == `test`;
foo == `test${1}`;
foo == NaN;
foo == Infinity;
typeof foo == "function";

({}) == foo;
`test${1}` == foo;

bar > 1;
bar < 1.2;
bar >= 1;
bar <= 1;
"#,
            r#"
foo === undefined;
foo !== null;
foo == 1;
foo != true;
foo == "str";
foo == `test`;
foo == `test${1}`;
foo == NaN;
foo == Infinity;
typeof foo == "function";
({}) == foo;
`test${1}` == foo;
bar > 1;
bar < 1.2;
bar >= 1;
bar <= 1;
"#,
        );
    }
}
