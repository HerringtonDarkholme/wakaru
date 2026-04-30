use oxc_ast::{ast::Expression, AstBuilder};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::GetSpan;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut normalizer = BooleanNormalizer {
        ast: AstBuilder::new(source.allocator),
    };

    normalizer.visit_program(&mut source.program);

    Ok(())
}

struct BooleanNormalizer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for BooleanNormalizer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        if let Some(value) = minified_boolean_value(expression) {
            let span = expression.span();
            *expression = self.ast.expression_boolean_literal(span, value);
            return;
        }

        walk_mut::walk_expression(self, expression);
    }
}

fn minified_boolean_value(expression: &Expression) -> Option<bool> {
    let Expression::UnaryExpression(unary) = expression else {
        return None;
    };

    if !unary.operator.is_not() {
        return None;
    }

    let Expression::NumericLiteral(argument) = &unary.argument else {
        return None;
    };

    match argument.value {
        0.0 => Some(true),
        1.0 => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn transforms_minified_booleans() {
        define_ast_inline_test(transform_ast)(
            "
let a = !1;
const b = !0;

var obj = {
  value: !0
};
",
            "
let a = false;
const b = true;
var obj = { value: true };
",
        );
    }

    #[test]
    fn leaves_other_logical_not_expressions_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
let a = !2;
let b = !foo;
let c = !!1;
",
            "
let a = !2;
let b = !foo;
let c = !false;
",
        );
    }
}
