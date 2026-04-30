use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{Argument, ArrayExpressionElement, Expression, TSTypeParameterInstantiation},
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::GetSpan;
use oxc_syntax::{
    number::NumberBase,
    operator::{BinaryOperator, UnaryOperator},
};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut restorer = TypeConstructorRestorer {
        ast: AstBuilder::new(source.allocator),
    };

    restorer.visit_program(&mut source.program);

    Ok(())
}

struct TypeConstructorRestorer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for TypeConstructorRestorer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        walk_mut::walk_expression(self, expression);

        if self.restore_number_constructor(expression) {
            return;
        }

        if self.restore_string_constructor(expression) {
            return;
        }

        self.restore_array_constructor(expression);
    }
}

impl<'a> TypeConstructorRestorer<'a> {
    fn restore_number_constructor(&self, expression: &mut Expression<'a>) -> bool {
        let Expression::UnaryExpression(unary) = expression else {
            return false;
        };

        if unary.operator != UnaryOperator::UnaryPlus
            || !matches!(unary.argument, Expression::Identifier(_))
        {
            return false;
        }

        let span = expression.span();
        let Expression::UnaryExpression(unary) = expression.take_in(self.ast) else {
            unreachable!();
        };
        let argument = unary.unbox().argument;
        *expression = self.call_expression(span, "Number", argument);

        true
    }

    fn restore_string_constructor(&self, expression: &mut Expression<'a>) -> bool {
        let Expression::BinaryExpression(binary) = expression else {
            return false;
        };

        if binary.operator != BinaryOperator::Addition || !is_empty_string_literal(&binary.right) {
            return false;
        }

        let span = expression.span();
        let Expression::BinaryExpression(binary) = expression.take_in(self.ast) else {
            unreachable!();
        };
        let left = binary.unbox().left;

        if matches!(left, Expression::StringLiteral(_)) {
            *expression = left;
        } else {
            *expression = self.call_expression(span, "String", left);
        }

        true
    }

    fn restore_array_constructor(&self, expression: &mut Expression<'a>) -> bool {
        let Expression::ArrayExpression(array) = expression else {
            return false;
        };

        if array.elements.is_empty()
            || !array
                .elements
                .iter()
                .all(|element| matches!(element, ArrayExpressionElement::Elision(_)))
        {
            return false;
        }

        let span = array.span;
        let length = array.elements.len() as f64;
        *expression = self.call_expression(
            span,
            "Array",
            self.ast
                .expression_numeric_literal(span, length, None, NumberBase::Decimal),
        );

        true
    }

    fn call_expression(
        &self,
        span: oxc_span::Span,
        callee_name: &'static str,
        argument: Expression<'a>,
    ) -> Expression<'a> {
        let mut arguments = self.ast.vec();
        arguments.push(expression_to_argument(argument));

        self.ast.expression_call(
            span,
            self.ast.expression_identifier(span, callee_name),
            None::<oxc_allocator::Box<'a, TSTypeParameterInstantiation<'a>>>,
            arguments,
            false,
        )
    }
}

fn is_empty_string_literal(expression: &Expression) -> bool {
    matches!(expression, Expression::StringLiteral(literal) if literal.value.as_str().is_empty())
}

fn expression_to_argument(expression: Expression) -> Argument {
    macro_rules! argument_variant {
        ($variant:ident, $value:ident) => {
            Argument::$variant($value)
        };
    }

    match expression {
        Expression::BooleanLiteral(value) => argument_variant!(BooleanLiteral, value),
        Expression::NullLiteral(value) => argument_variant!(NullLiteral, value),
        Expression::NumericLiteral(value) => argument_variant!(NumericLiteral, value),
        Expression::BigIntLiteral(value) => argument_variant!(BigIntLiteral, value),
        Expression::RegExpLiteral(value) => argument_variant!(RegExpLiteral, value),
        Expression::StringLiteral(value) => argument_variant!(StringLiteral, value),
        Expression::TemplateLiteral(value) => argument_variant!(TemplateLiteral, value),
        Expression::Identifier(value) => argument_variant!(Identifier, value),
        Expression::MetaProperty(value) => argument_variant!(MetaProperty, value),
        Expression::Super(value) => argument_variant!(Super, value),
        Expression::ArrayExpression(value) => argument_variant!(ArrayExpression, value),
        Expression::ArrowFunctionExpression(value) => {
            argument_variant!(ArrowFunctionExpression, value)
        }
        Expression::AssignmentExpression(value) => argument_variant!(AssignmentExpression, value),
        Expression::AwaitExpression(value) => argument_variant!(AwaitExpression, value),
        Expression::BinaryExpression(value) => argument_variant!(BinaryExpression, value),
        Expression::CallExpression(value) => argument_variant!(CallExpression, value),
        Expression::ChainExpression(value) => argument_variant!(ChainExpression, value),
        Expression::ClassExpression(value) => argument_variant!(ClassExpression, value),
        Expression::ConditionalExpression(value) => argument_variant!(ConditionalExpression, value),
        Expression::FunctionExpression(value) => argument_variant!(FunctionExpression, value),
        Expression::ImportExpression(value) => argument_variant!(ImportExpression, value),
        Expression::LogicalExpression(value) => argument_variant!(LogicalExpression, value),
        Expression::NewExpression(value) => argument_variant!(NewExpression, value),
        Expression::ObjectExpression(value) => argument_variant!(ObjectExpression, value),
        Expression::ParenthesizedExpression(value) => {
            argument_variant!(ParenthesizedExpression, value)
        }
        Expression::SequenceExpression(value) => argument_variant!(SequenceExpression, value),
        Expression::TaggedTemplateExpression(value) => {
            argument_variant!(TaggedTemplateExpression, value)
        }
        Expression::ThisExpression(value) => argument_variant!(ThisExpression, value),
        Expression::UnaryExpression(value) => argument_variant!(UnaryExpression, value),
        Expression::UpdateExpression(value) => argument_variant!(UpdateExpression, value),
        Expression::YieldExpression(value) => argument_variant!(YieldExpression, value),
        Expression::PrivateInExpression(value) => argument_variant!(PrivateInExpression, value),
        Expression::JSXElement(value) => argument_variant!(JSXElement, value),
        Expression::JSXFragment(value) => argument_variant!(JSXFragment, value),
        Expression::TSAsExpression(value) => argument_variant!(TSAsExpression, value),
        Expression::TSSatisfiesExpression(value) => argument_variant!(TSSatisfiesExpression, value),
        Expression::TSTypeAssertion(value) => argument_variant!(TSTypeAssertion, value),
        Expression::TSNonNullExpression(value) => argument_variant!(TSNonNullExpression, value),
        Expression::TSInstantiationExpression(value) => {
            argument_variant!(TSInstantiationExpression, value)
        }
        Expression::ComputedMemberExpression(value) => {
            argument_variant!(ComputedMemberExpression, value)
        }
        Expression::StaticMemberExpression(value) => {
            argument_variant!(StaticMemberExpression, value)
        }
        Expression::PrivateFieldExpression(value) => {
            argument_variant!(PrivateFieldExpression, value)
        }
        Expression::V8IntrinsicExpression(value) => argument_variant!(V8IntrinsicExpression, value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn restores_type_constructors_from_minified_code() {
        define_ast_inline_test(transform_ast)(
            r#"
+x;
x + "";
[,,,];
"#,
            r#"
Number(x);
String(x);
Array(3);
"#,
        );
    }

    #[test]
    fn handles_complex_constructor_shapes() {
        define_ast_inline_test(transform_ast)(
            r#"
var a = 6 + +x;
var b = x + "a";
var c = 'long string' + x + '';
var d = x + 5 + '';
var e = x + '' + 5;
var f = 'str' + x + '' + 5 + '' + 6;
var g = 'str' + '';

function foo(numStr, result) {
    var num = +numStr;
    var arr = [,,,].fill(num + '').join(' + ');
    return `${result} = ${arr}`;
}

const emptyArr = [];
const oneArr = [,];
"#,
            r#"
var a = 6 + Number(x);
var b = x + "a";
var c = String("long string" + x);
var d = String(x + 5);
var e = String(x) + 5;
var f = String(String("str" + x) + 5) + 6;
var g = "str";
function foo(numStr, result) {
  var num = Number(numStr);
  var arr = Array(3).fill(String(num)).join(" + ");
  return `${result} = ${arr}`;
}
const emptyArr = [];
const oneArr = Array(1);
"#,
        );
    }

    #[test]
    fn leaves_unsupported_constructor_like_shapes() {
        define_ast_inline_test(transform_ast)(
            r#"
+1n;
+(x);
!!x;
x + "a";
[];
[1,,];
"#,
            r#"
+1n;
+x;
!!x;
x + "a";
[];
[1, ,];
"#,
        );
    }
}
