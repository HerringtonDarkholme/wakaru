use oxc_allocator::CloneIn;
use oxc_ast::{
    ast::{Argument, CallExpression, Expression},
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::{ContentEq, GetSpan};
use oxc_syntax::operator::UnaryOperator;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut transformer = ArgumentSpreadTransformer {
        ast: AstBuilder::new(source.allocator),
    };

    transformer.visit_program(&mut source.program);

    Ok(())
}

struct ArgumentSpreadTransformer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for ArgumentSpreadTransformer<'a> {
    fn visit_call_expression(&mut self, call: &mut CallExpression<'a>) {
        walk_mut::walk_call_expression(self, call);

        if let Some((callee, spread_argument)) = self.replacement(call) {
            let span = spread_argument.span();
            let mut arguments = self.ast.vec_with_capacity(1);
            arguments.push(self.ast.argument_spread_element(span, spread_argument));

            call.callee = callee;
            call.arguments = arguments;
            call.type_arguments = None;
            call.optional = false;
        }
    }
}

impl<'a> ArgumentSpreadTransformer<'a> {
    fn replacement(&self, call: &CallExpression<'a>) -> Option<(Expression<'a>, Expression<'a>)> {
        if call.arguments.len() != 2
            || matches!(call.arguments.first(), Some(Argument::SpreadElement(_)))
            || matches!(call.arguments.get(1), Some(Argument::SpreadElement(_)))
        {
            return None;
        }

        let Expression::StaticMemberExpression(apply_member) = without_parentheses(&call.callee)
        else {
            return None;
        };
        if apply_member.property.name != "apply" {
            return None;
        }

        if let Some(callee) = function_apply_callee(&apply_member.object, &call.arguments[0]) {
            let spread_argument =
                argument_to_expression(call.arguments[1].clone_in(self.ast.allocator))?;
            return Some((callee.clone_in(self.ast.allocator), spread_argument));
        }

        let object_apply_callee = match without_parentheses(&apply_member.object) {
            Expression::StaticMemberExpression(member) => {
                let this_arg =
                    argument_to_expression(call.arguments[0].clone_in(self.ast.allocator))?;
                if !member.object.content_eq(&this_arg) {
                    return None;
                }
                Some(Expression::StaticMemberExpression(
                    member.clone_in(self.ast.allocator),
                ))
            }
            Expression::ComputedMemberExpression(member) => {
                let this_arg =
                    argument_to_expression(call.arguments[0].clone_in(self.ast.allocator))?;
                if !member.object.content_eq(&this_arg) {
                    return None;
                }
                Some(Expression::ComputedMemberExpression(
                    member.clone_in(self.ast.allocator),
                ))
            }
            _ => None,
        }?;

        let spread_argument =
            argument_to_expression(call.arguments[1].clone_in(self.ast.allocator))?;
        Some((object_apply_callee, spread_argument))
    }
}

fn function_apply_callee<'a, 'b>(
    apply_object: &'b Expression<'a>,
    this_arg: &Argument<'a>,
) -> Option<&'b Expression<'a>> {
    if !matches!(without_parentheses(apply_object), Expression::Identifier(_)) {
        return None;
    }

    if is_null_argument(this_arg) || is_undefined_argument(this_arg) {
        Some(apply_object)
    } else {
        None
    }
}

fn is_null_argument(argument: &Argument) -> bool {
    matches!(argument, Argument::NullLiteral(_))
}

fn is_undefined_argument(argument: &Argument) -> bool {
    match argument {
        Argument::Identifier(identifier) => identifier.name == "undefined",
        Argument::UnaryExpression(unary) => {
            unary.operator == UnaryOperator::Void
                && matches!(&unary.argument, Expression::NumericLiteral(number) if number.value == 0.0)
        }
        _ => false,
    }
}

fn without_parentheses<'a, 'b>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            without_parentheses(&parenthesized.expression)
        }
        _ => expression,
    }
}

fn argument_to_expression(argument: Argument) -> Option<Expression> {
    macro_rules! expression_variant {
        ($variant:ident, $value:ident) => {
            Some(Expression::$variant($value))
        };
    }

    match argument {
        Argument::SpreadElement(_) => None,
        Argument::BooleanLiteral(value) => expression_variant!(BooleanLiteral, value),
        Argument::NullLiteral(value) => expression_variant!(NullLiteral, value),
        Argument::NumericLiteral(value) => expression_variant!(NumericLiteral, value),
        Argument::BigIntLiteral(value) => expression_variant!(BigIntLiteral, value),
        Argument::RegExpLiteral(value) => expression_variant!(RegExpLiteral, value),
        Argument::StringLiteral(value) => expression_variant!(StringLiteral, value),
        Argument::TemplateLiteral(value) => expression_variant!(TemplateLiteral, value),
        Argument::Identifier(value) => expression_variant!(Identifier, value),
        Argument::MetaProperty(value) => expression_variant!(MetaProperty, value),
        Argument::Super(value) => expression_variant!(Super, value),
        Argument::ArrayExpression(value) => expression_variant!(ArrayExpression, value),
        Argument::ArrowFunctionExpression(value) => {
            expression_variant!(ArrowFunctionExpression, value)
        }
        Argument::AssignmentExpression(value) => expression_variant!(AssignmentExpression, value),
        Argument::AwaitExpression(value) => expression_variant!(AwaitExpression, value),
        Argument::BinaryExpression(value) => expression_variant!(BinaryExpression, value),
        Argument::CallExpression(value) => expression_variant!(CallExpression, value),
        Argument::ChainExpression(value) => expression_variant!(ChainExpression, value),
        Argument::ClassExpression(value) => expression_variant!(ClassExpression, value),
        Argument::ConditionalExpression(value) => expression_variant!(ConditionalExpression, value),
        Argument::FunctionExpression(value) => expression_variant!(FunctionExpression, value),
        Argument::ImportExpression(value) => expression_variant!(ImportExpression, value),
        Argument::LogicalExpression(value) => expression_variant!(LogicalExpression, value),
        Argument::NewExpression(value) => expression_variant!(NewExpression, value),
        Argument::ObjectExpression(value) => expression_variant!(ObjectExpression, value),
        Argument::ParenthesizedExpression(value) => {
            expression_variant!(ParenthesizedExpression, value)
        }
        Argument::SequenceExpression(value) => expression_variant!(SequenceExpression, value),
        Argument::TaggedTemplateExpression(value) => {
            expression_variant!(TaggedTemplateExpression, value)
        }
        Argument::ThisExpression(value) => expression_variant!(ThisExpression, value),
        Argument::UnaryExpression(value) => expression_variant!(UnaryExpression, value),
        Argument::UpdateExpression(value) => expression_variant!(UpdateExpression, value),
        Argument::YieldExpression(value) => expression_variant!(YieldExpression, value),
        Argument::PrivateInExpression(value) => expression_variant!(PrivateInExpression, value),
        Argument::JSXElement(value) => expression_variant!(JSXElement, value),
        Argument::JSXFragment(value) => expression_variant!(JSXFragment, value),
        Argument::TSAsExpression(value) => expression_variant!(TSAsExpression, value),
        Argument::TSSatisfiesExpression(value) => {
            expression_variant!(TSSatisfiesExpression, value)
        }
        Argument::TSTypeAssertion(value) => expression_variant!(TSTypeAssertion, value),
        Argument::TSNonNullExpression(value) => expression_variant!(TSNonNullExpression, value),
        Argument::TSInstantiationExpression(value) => {
            expression_variant!(TSInstantiationExpression, value)
        }
        Argument::ComputedMemberExpression(value) => {
            expression_variant!(ComputedMemberExpression, value)
        }
        Argument::StaticMemberExpression(value) => {
            expression_variant!(StaticMemberExpression, value)
        }
        Argument::PrivateFieldExpression(value) => {
            expression_variant!(PrivateFieldExpression, value)
        }
        Argument::V8IntrinsicExpression(value) => expression_variant!(V8IntrinsicExpression, value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn converts_plain_function_apply() {
        define_ast_inline_test(transform_ast)(
            "
fn.apply(undefined, someArray);
fn.apply(null, someArray);
fn.apply(void 0, someArray);
",
            "
fn(...someArray);
fn(...someArray);
fn(...someArray);
",
        );
    }

    #[test]
    fn leaves_plain_apply_with_real_this_arg() {
        define_ast_inline_test(transform_ast)(
            "
fn.apply(obj, someArray);
fn.apply(this, someArray);
fn.apply({}, someArray);
",
            "
fn.apply(obj, someArray);
fn.apply(this, someArray);
fn.apply({}, someArray);
",
        );
    }

    #[test]
    fn converts_object_member_apply() {
        define_ast_inline_test(transform_ast)(
            "
obj.fn.apply(obj, someArray);
obj[fn].apply(obj, someArray);

class T {
  fn() {
    this.fn.apply(this, someArray);
  }
}
",
            "
obj.fn(...someArray);
obj[fn](...someArray);
class T {
  fn() {
    this.fn(...someArray);
  }
}
",
        );
    }

    #[test]
    fn converts_matching_complex_receiver_apply() {
        define_ast_inline_test(transform_ast)(
            "
foo[bar+1].baz.fn.apply(foo[bar+1].baz, someArray);
[].fn.apply([], someArray);
",
            "
foo[bar + 1].baz.fn(...someArray);
[].fn(...someArray);
",
        );
    }

    #[test]
    fn leaves_object_apply_without_matching_receiver() {
        define_ast_inline_test(transform_ast)(
            "
obj.fn.apply(otherObj, someArray);
obj.fn.apply(undefined, someArray);
obj.fn.apply(void 0, someArray);
obj.fn.apply(null, someArray);
obj.fn.apply(this, someArray);
obj.fn.apply({}, someArray);
",
            "
obj.fn.apply(otherObj, someArray);
obj.fn.apply(undefined, someArray);
obj.fn.apply(void 0, someArray);
obj.fn.apply(null, someArray);
obj.fn.apply(this, someArray);
obj.fn.apply({}, someArray);
",
        );
    }
}
