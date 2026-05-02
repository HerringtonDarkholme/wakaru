use oxc_allocator::CloneIn;
use oxc_ast::{
    ast::{
        Argument, Expression, JSXAttributeItem, JSXAttributeValue, JSXChild, JSXElementName,
        JSXExpression, JSXMemberExpressionObject, ObjectPropertyKind, PropertyKey,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::GetSpan;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut transformer = JsxTransformer {
        ast: AstBuilder::new(source.allocator),
    };

    transformer.visit_program(&mut source.program);

    Ok(())
}

struct JsxTransformer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for JsxTransformer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        walk_mut::walk_expression(self, expression);

        if let Some(jsx) = self.to_jsx_expression(expression) {
            *expression = jsx;
        }
    }
}

impl<'a> JsxTransformer<'a> {
    fn to_jsx_expression(&self, expression: &Expression<'a>) -> Option<Expression<'a>> {
        let Expression::CallExpression(call) = without_parentheses(expression) else {
            return None;
        };
        if !is_create_element_call(&call.callee) || call.arguments.len() < 2 {
            return None;
        }

        let tag = self.to_jsx_tag(argument_to_expression(
            call.arguments[0].clone_in(self.ast.allocator),
        )?)?;
        if capitalization_invalid(&tag) {
            return None;
        }

        let attributes = self.to_jsx_attributes(&call.arguments[1])?;
        let mut children = self.ast.vec();
        for child in call.arguments.iter().skip(2) {
            if let Some(child) = self.to_jsx_child(child) {
                children.push(child);
            }
        }

        let span = call.span;
        let closing_element = if children.is_empty() {
            None
        } else {
            Some(
                self.ast
                    .jsx_closing_element(span, tag.clone_in(self.ast.allocator)),
            )
        };
        let opening_element =
            self.ast
                .jsx_opening_element(span, tag, None::<oxc_allocator::Box<'a, _>>, attributes);

        Some(Expression::JSXElement(self.ast.alloc_jsx_element(
            span,
            opening_element,
            children,
            closing_element,
        )))
    }

    fn to_jsx_tag(&self, expression: Expression<'a>) -> Option<JSXElementName<'a>> {
        match expression {
            Expression::StringLiteral(literal) => Some(
                self.ast
                    .jsx_element_name_identifier(literal.span, literal.value.as_str()),
            ),
            Expression::Identifier(identifier) => {
                Some(self.ast.jsx_element_name_identifier_reference(
                    identifier.span,
                    identifier.name.as_str(),
                ))
            }
            Expression::StaticMemberExpression(member) => {
                let object =
                    self.to_jsx_member_object(member.object.clone_in(self.ast.allocator))?;
                let property = self
                    .ast
                    .jsx_identifier(member.property.span, member.property.name.as_str());
                Some(
                    self.ast
                        .jsx_element_name_member_expression(member.span, object, property),
                )
            }
            _ => None,
        }
    }

    fn to_jsx_member_object(
        &self,
        expression: Expression<'a>,
    ) -> Option<JSXMemberExpressionObject<'a>> {
        match expression {
            Expression::Identifier(identifier) => {
                Some(self.ast.jsx_member_expression_object_identifier_reference(
                    identifier.span,
                    identifier.name.as_str(),
                ))
            }
            Expression::StaticMemberExpression(member) => {
                let object =
                    self.to_jsx_member_object(member.object.clone_in(self.ast.allocator))?;
                let property = self
                    .ast
                    .jsx_identifier(member.property.span, member.property.name.as_str());
                Some(self.ast.jsx_member_expression_object_member_expression(
                    member.span,
                    object,
                    property,
                ))
            }
            _ => None,
        }
    }

    fn to_jsx_attributes(
        &self,
        props: &Argument<'a>,
    ) -> Option<oxc_allocator::Vec<'a, JSXAttributeItem<'a>>> {
        if matches!(props, Argument::NullLiteral(_)) {
            return Some(self.ast.vec());
        }

        if let Argument::SpreadElement(spread) = props {
            let mut attributes = self.ast.vec_with_capacity(1);
            attributes.push(self.ast.jsx_attribute_item_spread_attribute(
                spread.span,
                spread.argument.clone_in(self.ast.allocator),
            ));
            return Some(attributes);
        }

        let expression = argument_to_expression(props.clone_in(self.ast.allocator))?;
        match expression {
            Expression::ObjectExpression(object) => {
                let mut attributes = self.ast.vec();
                for property in &object.properties {
                    match property {
                        ObjectPropertyKind::SpreadProperty(spread) => {
                            attributes.push(self.ast.jsx_attribute_item_spread_attribute(
                                spread.span,
                                spread.argument.clone_in(self.ast.allocator),
                            ));
                        }
                        ObjectPropertyKind::ObjectProperty(property) => {
                            let Some(name) = property_key_name(&property.key) else {
                                return None;
                            };

                            let name = self.ast.jsx_attribute_name_identifier(
                                property.key.span(),
                                self.ast.str(name),
                            );
                            let value = if is_true_literal(&property.value) {
                                None
                            } else {
                                Some(self.to_jsx_attribute_value(&property.value))
                            };
                            attributes.push(self.ast.jsx_attribute_item_attribute(
                                property.span,
                                name,
                                value,
                            ));
                        }
                    }
                }
                Some(attributes)
            }
            expression => {
                let mut attributes = self.ast.vec_with_capacity(1);
                attributes.push(
                    self.ast
                        .jsx_attribute_item_spread_attribute(expression.span(), expression),
                );
                Some(attributes)
            }
        }
    }

    fn to_jsx_attribute_value(&self, expression: &Expression<'a>) -> JSXAttributeValue<'a> {
        if let Expression::StringLiteral(string) = expression {
            if can_be_attribute_string(string.value.as_str()) {
                return self.ast.jsx_attribute_value_string_literal(
                    string.span,
                    string.value.as_str(),
                    string.raw,
                );
            }
        }

        self.ast.jsx_attribute_value_expression_container(
            expression.span(),
            expression_to_jsx_expression(expression.clone_in(self.ast.allocator)),
        )
    }

    fn to_jsx_child(&self, argument: &Argument<'a>) -> Option<JSXChild<'a>> {
        if matches!(
            argument,
            Argument::NullLiteral(_) | Argument::BooleanLiteral(_)
        ) || is_undefined_argument(argument)
        {
            return None;
        }

        if let Argument::SpreadElement(spread) = argument {
            return Some(
                self.ast
                    .jsx_child_spread(spread.span, spread.argument.clone_in(self.ast.allocator)),
            );
        }

        let expression = argument_to_expression(argument.clone_in(self.ast.allocator))?;
        match expression {
            Expression::JSXElement(element) => Some(JSXChild::Element(element)),
            Expression::JSXFragment(fragment) => Some(JSXChild::Fragment(fragment)),
            Expression::StringLiteral(string) if can_be_text_child(string.value.as_str()) => Some(
                self.ast
                    .jsx_child_text(string.span, string.value.as_str(), None),
            ),
            expression => Some(self.ast.jsx_child_expression_container(
                expression.span(),
                expression_to_jsx_expression(expression),
            )),
        }
    }
}

fn is_create_element_call(callee: &Expression) -> bool {
    match without_parentheses(callee) {
        Expression::Identifier(identifier) => identifier.name == "createElement",
        Expression::StaticMemberExpression(member) => {
            if member.property.name != "createElement" {
                return false;
            }
            !matches!(&member.object, Expression::Identifier(object) if object.name == "document")
        }
        _ => false,
    }
}

fn capitalization_invalid(tag: &JSXElementName) -> bool {
    match tag {
        JSXElementName::Identifier(identifier) => identifier
            .name
            .as_str()
            .chars()
            .next()
            .is_some_and(|ch| !ch.is_ascii_lowercase()),
        JSXElementName::IdentifierReference(identifier) => identifier
            .name
            .as_str()
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase()),
        _ => false,
    }
}

fn property_key_name<'a>(key: &'a PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(identifier) => Some(identifier.name.as_str()),
        PropertyKey::StringLiteral(string) => Some(string.value.as_str()),
        _ => None,
    }
}

fn can_be_attribute_string(value: &str) -> bool {
    !value.contains('\\') && !value.contains('"')
}

fn can_be_text_child(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['{', '}', '<', '>', '\r', '\n'])
        && !value.starts_with(char::is_whitespace)
        && !value.ends_with(char::is_whitespace)
}

fn is_true_literal(expression: &Expression) -> bool {
    matches!(expression, Expression::BooleanLiteral(boolean) if boolean.value)
}

fn is_undefined_argument(argument: &Argument) -> bool {
    match argument {
        Argument::Identifier(identifier) => identifier.name == "undefined",
        Argument::UnaryExpression(unary) => {
            unary.operator == oxc_syntax::operator::UnaryOperator::Void
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

fn expression_to_jsx_expression(expression: Expression) -> JSXExpression {
    macro_rules! jsx_expression_variant {
        ($variant:ident, $value:ident) => {
            JSXExpression::$variant($value)
        };
    }

    match expression {
        Expression::BooleanLiteral(value) => jsx_expression_variant!(BooleanLiteral, value),
        Expression::NullLiteral(value) => jsx_expression_variant!(NullLiteral, value),
        Expression::NumericLiteral(value) => jsx_expression_variant!(NumericLiteral, value),
        Expression::BigIntLiteral(value) => jsx_expression_variant!(BigIntLiteral, value),
        Expression::RegExpLiteral(value) => jsx_expression_variant!(RegExpLiteral, value),
        Expression::StringLiteral(value) => jsx_expression_variant!(StringLiteral, value),
        Expression::TemplateLiteral(value) => jsx_expression_variant!(TemplateLiteral, value),
        Expression::Identifier(value) => jsx_expression_variant!(Identifier, value),
        Expression::MetaProperty(value) => jsx_expression_variant!(MetaProperty, value),
        Expression::Super(value) => jsx_expression_variant!(Super, value),
        Expression::ArrayExpression(value) => jsx_expression_variant!(ArrayExpression, value),
        Expression::ArrowFunctionExpression(value) => {
            jsx_expression_variant!(ArrowFunctionExpression, value)
        }
        Expression::AssignmentExpression(value) => {
            jsx_expression_variant!(AssignmentExpression, value)
        }
        Expression::AwaitExpression(value) => jsx_expression_variant!(AwaitExpression, value),
        Expression::BinaryExpression(value) => jsx_expression_variant!(BinaryExpression, value),
        Expression::CallExpression(value) => jsx_expression_variant!(CallExpression, value),
        Expression::ChainExpression(value) => jsx_expression_variant!(ChainExpression, value),
        Expression::ClassExpression(value) => jsx_expression_variant!(ClassExpression, value),
        Expression::ConditionalExpression(value) => {
            jsx_expression_variant!(ConditionalExpression, value)
        }
        Expression::FunctionExpression(value) => {
            jsx_expression_variant!(FunctionExpression, value)
        }
        Expression::ImportExpression(value) => jsx_expression_variant!(ImportExpression, value),
        Expression::LogicalExpression(value) => jsx_expression_variant!(LogicalExpression, value),
        Expression::NewExpression(value) => jsx_expression_variant!(NewExpression, value),
        Expression::ObjectExpression(value) => jsx_expression_variant!(ObjectExpression, value),
        Expression::ParenthesizedExpression(value) => {
            jsx_expression_variant!(ParenthesizedExpression, value)
        }
        Expression::SequenceExpression(value) => jsx_expression_variant!(SequenceExpression, value),
        Expression::TaggedTemplateExpression(value) => {
            jsx_expression_variant!(TaggedTemplateExpression, value)
        }
        Expression::ThisExpression(value) => jsx_expression_variant!(ThisExpression, value),
        Expression::UnaryExpression(value) => jsx_expression_variant!(UnaryExpression, value),
        Expression::UpdateExpression(value) => jsx_expression_variant!(UpdateExpression, value),
        Expression::YieldExpression(value) => jsx_expression_variant!(YieldExpression, value),
        Expression::PrivateInExpression(value) => {
            jsx_expression_variant!(PrivateInExpression, value)
        }
        Expression::JSXElement(value) => jsx_expression_variant!(JSXElement, value),
        Expression::JSXFragment(value) => jsx_expression_variant!(JSXFragment, value),
        Expression::TSAsExpression(value) => jsx_expression_variant!(TSAsExpression, value),
        Expression::TSSatisfiesExpression(value) => {
            jsx_expression_variant!(TSSatisfiesExpression, value)
        }
        Expression::TSTypeAssertion(value) => jsx_expression_variant!(TSTypeAssertion, value),
        Expression::TSNonNullExpression(value) => {
            jsx_expression_variant!(TSNonNullExpression, value)
        }
        Expression::TSInstantiationExpression(value) => {
            jsx_expression_variant!(TSInstantiationExpression, value)
        }
        Expression::ComputedMemberExpression(value) => {
            jsx_expression_variant!(ComputedMemberExpression, value)
        }
        Expression::StaticMemberExpression(value) => {
            jsx_expression_variant!(StaticMemberExpression, value)
        }
        Expression::PrivateFieldExpression(value) => {
            jsx_expression_variant!(PrivateFieldExpression, value)
        }
        Expression::V8IntrinsicExpression(value) => {
            jsx_expression_variant!(V8IntrinsicExpression, value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn restores_simple_classic_jsx() {
        define_ast_inline_test(transform_ast)(
            r#"
function fn() {
  return React.createElement("div", {
    className: "flex flex-col",
    num: 1,
    foo: bar,
    disabled: true,
  });
}
"#,
            r#"
function fn() {
  return <div className="flex flex-col" num={1} foo={bar} disabled />;
}
"#,
        );
    }

    #[test]
    fn restores_children_and_nested_elements() {
        define_ast_inline_test(transform_ast)(
            r#"
function fn() {
  return React.createElement("div", null, child, React.createElement("span", null, "Hello"));
}
"#,
            r#"
function fn() {
  return <div>{child}<span>Hello</span></div>;
}
"#,
        );
    }

    #[test]
    fn handles_component_member_and_spread_props() {
        define_ast_inline_test(transform_ast)(
            r#"
React.createElement(Button, { variant: "contained" }, "Hello");
React.createElement(mui.Button, { ...props, foo: "bar" });
React.createElement("div", wrap(props));
"#,
            r#"
<Button variant="contained">Hello</Button>;
<mui.Button {...props} foo="bar" />;
<div {...wrap(props)} />;
"#,
        );
    }

    #[test]
    fn leaves_bad_capitalization_and_document_create_element() {
        define_ast_inline_test(transform_ast)(
            r#"
React.createElement(foo, null);
React.createElement("Foo", null);
document.createElement("div", null);
"#,
            r#"
React.createElement(foo, null);
React.createElement("Foo", null);
document.createElement("div", null);
"#,
        );
    }
}
