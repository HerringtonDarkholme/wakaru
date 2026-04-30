use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{Argument, CallExpression, Expression, StaticMemberExpression, TemplateElementValue},
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::{GetSpan, Span};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut restorer = TemplateLiteralRestorer {
        ast: AstBuilder::new(source.allocator),
    };

    restorer.visit_program(&mut source.program);

    Ok(())
}

struct TemplateLiteralRestorer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for TemplateLiteralRestorer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        if is_concat_chain(expression) {
            let span = expression.span();
            let old_expression = expression.take_in(self.ast);
            if let Some(parts) = collect_concat_chain(old_expression) {
                *expression = self.template_literal_expression(span, parts);
                return;
            }
        }

        walk_mut::walk_expression(self, expression);
    }
}

impl<'a> TemplateLiteralRestorer<'a> {
    fn template_literal_expression(
        &self,
        span: Span,
        parts: std::vec::Vec<TemplatePart<'a>>,
    ) -> Expression<'a> {
        let mut quasis = self.ast.vec();
        let mut expressions = self.ast.vec();
        let mut current_quasi = String::new();

        for part in parts {
            match part {
                TemplatePart::Text(text) => current_quasi.push_str(&escape_template_text(&text)),
                TemplatePart::Expression(expression) => {
                    quasis.push(self.template_element(span, &current_quasi, false));
                    expressions.push(expression);
                    current_quasi.clear();
                }
            }
        }

        quasis.push(self.template_element(span, &current_quasi, true));
        self.ast
            .expression_template_literal(span, quasis, expressions)
    }

    fn template_element(
        &self,
        span: Span,
        raw: &str,
        tail: bool,
    ) -> oxc_ast::ast::TemplateElement<'a> {
        let raw = self.ast.str(raw);
        let value = TemplateElementValue {
            raw,
            cooked: Some(raw),
        };

        self.ast.template_element(span, value, tail, false)
    }
}

enum TemplatePart<'a> {
    Text(String),
    Expression(Expression<'a>),
}

fn is_concat_chain(expression: &Expression) -> bool {
    let Expression::CallExpression(call) = expression else {
        return false;
    };

    is_concat_call(call) && concat_chain_starts_with_string(&call.callee)
}

fn concat_chain_starts_with_string(callee: &Expression) -> bool {
    let Some(object) = concat_call_object(callee) else {
        return false;
    };

    match object {
        Expression::StringLiteral(_) => true,
        Expression::CallExpression(call) => {
            is_concat_call(call) && concat_chain_starts_with_string(&call.callee)
        }
        _ => false,
    }
}

fn collect_concat_chain(expression: Expression) -> Option<std::vec::Vec<TemplatePart>> {
    let Expression::CallExpression(call) = expression else {
        return None;
    };

    collect_concat_call(call)
}

fn collect_concat_call<'a>(
    call: oxc_allocator::Box<'a, CallExpression<'a>>,
) -> Option<std::vec::Vec<TemplatePart<'a>>> {
    let CallExpression {
        callee, arguments, ..
    } = call.unbox();
    let object = concat_call_object_owned(callee)?;

    let mut parts = match object {
        Expression::StringLiteral(literal) => {
            vec![TemplatePart::Text(literal.value.as_str().to_string())]
        }
        Expression::CallExpression(call) => collect_concat_call(call)?,
        _ => return None,
    };

    for argument in arguments {
        parts.push(argument_to_template_part(argument)?);
    }

    Some(parts)
}

fn concat_call_object<'a, 'b>(callee: &'b Expression<'a>) -> Option<&'b Expression<'a>> {
    let Expression::StaticMemberExpression(member) = callee else {
        return None;
    };

    if !is_concat_member(member) {
        return None;
    }

    Some(&member.object)
}

fn concat_call_object_owned(callee: Expression) -> Option<Expression> {
    let Expression::StaticMemberExpression(member) = callee else {
        return None;
    };

    if !is_concat_member(&member) {
        return None;
    }

    let StaticMemberExpression { object, .. } = member.unbox();
    Some(object)
}

fn is_concat_call(call: &CallExpression) -> bool {
    concat_call_object(&call.callee).is_some()
}

fn is_concat_member(member: &StaticMemberExpression) -> bool {
    !member.optional && member.property.name.as_str() == "concat"
}

fn argument_to_template_part(argument: Argument) -> Option<TemplatePart> {
    macro_rules! expression_part {
        ($variant:ident, $value:ident) => {
            Some(TemplatePart::Expression(Expression::$variant($value)))
        };
    }

    match argument {
        Argument::StringLiteral(literal) => {
            Some(TemplatePart::Text(literal.value.as_str().to_string()))
        }
        Argument::SpreadElement(_) => None,
        Argument::BooleanLiteral(value) => expression_part!(BooleanLiteral, value),
        Argument::NullLiteral(value) => expression_part!(NullLiteral, value),
        Argument::NumericLiteral(value) => expression_part!(NumericLiteral, value),
        Argument::BigIntLiteral(value) => expression_part!(BigIntLiteral, value),
        Argument::RegExpLiteral(value) => expression_part!(RegExpLiteral, value),
        Argument::TemplateLiteral(value) => expression_part!(TemplateLiteral, value),
        Argument::Identifier(value) => expression_part!(Identifier, value),
        Argument::MetaProperty(value) => expression_part!(MetaProperty, value),
        Argument::Super(value) => expression_part!(Super, value),
        Argument::ArrayExpression(value) => expression_part!(ArrayExpression, value),
        Argument::ArrowFunctionExpression(value) => {
            expression_part!(ArrowFunctionExpression, value)
        }
        Argument::AssignmentExpression(value) => expression_part!(AssignmentExpression, value),
        Argument::AwaitExpression(value) => expression_part!(AwaitExpression, value),
        Argument::BinaryExpression(value) => expression_part!(BinaryExpression, value),
        Argument::CallExpression(value) => expression_part!(CallExpression, value),
        Argument::ChainExpression(value) => expression_part!(ChainExpression, value),
        Argument::ClassExpression(value) => expression_part!(ClassExpression, value),
        Argument::ConditionalExpression(value) => expression_part!(ConditionalExpression, value),
        Argument::FunctionExpression(value) => expression_part!(FunctionExpression, value),
        Argument::ImportExpression(value) => expression_part!(ImportExpression, value),
        Argument::LogicalExpression(value) => expression_part!(LogicalExpression, value),
        Argument::NewExpression(value) => expression_part!(NewExpression, value),
        Argument::ObjectExpression(value) => expression_part!(ObjectExpression, value),
        Argument::ParenthesizedExpression(value) => {
            expression_part!(ParenthesizedExpression, value)
        }
        Argument::SequenceExpression(value) => expression_part!(SequenceExpression, value),
        Argument::TaggedTemplateExpression(value) => {
            expression_part!(TaggedTemplateExpression, value)
        }
        Argument::ThisExpression(value) => expression_part!(ThisExpression, value),
        Argument::UnaryExpression(value) => expression_part!(UnaryExpression, value),
        Argument::UpdateExpression(value) => expression_part!(UpdateExpression, value),
        Argument::YieldExpression(value) => expression_part!(YieldExpression, value),
        Argument::PrivateInExpression(value) => expression_part!(PrivateInExpression, value),
        Argument::JSXElement(value) => expression_part!(JSXElement, value),
        Argument::JSXFragment(value) => expression_part!(JSXFragment, value),
        Argument::TSAsExpression(value) => expression_part!(TSAsExpression, value),
        Argument::TSSatisfiesExpression(value) => expression_part!(TSSatisfiesExpression, value),
        Argument::TSTypeAssertion(value) => expression_part!(TSTypeAssertion, value),
        Argument::TSNonNullExpression(value) => expression_part!(TSNonNullExpression, value),
        Argument::TSInstantiationExpression(value) => {
            expression_part!(TSInstantiationExpression, value)
        }
        Argument::ComputedMemberExpression(value) => {
            expression_part!(ComputedMemberExpression, value)
        }
        Argument::StaticMemberExpression(value) => expression_part!(StaticMemberExpression, value),
        Argument::PrivateFieldExpression(value) => expression_part!(PrivateFieldExpression, value),
        Argument::V8IntrinsicExpression(value) => expression_part!(V8IntrinsicExpression, value),
    }
}

fn escape_template_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());

    for character in value.chars() {
        match character {
            '\n' => escaped.push_str("\\n"),
            '\t' => escaped.push_str("\\t"),
            '\r' => escaped.push_str("\\r"),
            '\u{0008}' => escaped.push_str("\\b"),
            '\u{000C}' => escaped.push_str("\\f"),
            '\u{000B}' => escaped.push_str("\\v"),
            '\0' => escaped.push_str("\\0"),
            '`' => escaped.push_str("\\`"),
            '$' => escaped.push_str("\\$"),
            '\\' => escaped.push_str("\\\\"),
            _ => escaped.push(character),
        }
    }

    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn restores_template_literal_syntax() {
        define_ast_inline_test(transform_ast)(
            r#"
var example1 = "the ".concat("simple ", form);
var example2 = "".concat(1);
var example3 = 1 + "".concat(foo).concat(bar).concat(baz);
var example4 = 1 + "".concat(foo, "bar").concat(baz);
var example5 = "".concat(1, f, "oo", true).concat(b, "ar", 0).concat(baz);
var example6 = "test ".concat(foo, " ").concat(bar);
"#,
            r#"
var example1 = `the simple ${form}`;
var example2 = `${1}`;
var example3 = 1 + `${foo}${bar}${baz}`;
var example4 = 1 + `${foo}bar${baz}`;
var example5 = `${1}${f}oo${true}${b}ar${0}${baz}`;
var example6 = `test ${foo} ${bar}`;
"#,
        );
    }

    #[test]
    fn restores_multiple_arguments() {
        define_ast_inline_test(transform_ast)(
            r#"
"the ".concat(first, " take the ").concat(second, " and ").concat(third);
"#,
            r#"
`the ${first} take the ${second} and ${third}`;
"#,
        );
    }

    #[test]
    fn handles_escaped_quotes() {
        define_ast_inline_test(transform_ast)(
            r#"
"'".concat(foo, "' \"").concat(bar, "\"");
"#,
            r#"
`'${foo}' "${bar}"`;
"#,
        );
    }

    #[test]
    fn escapes_backticks_and_dollar_signs() {
        define_ast_inline_test(transform_ast)(
            r#"
const codeBlock = "```".concat(lang, "\n").concat(code, "\n```");
"#,
            r#"
const codeBlock = `\`\`\`${lang}\n${code}\n\`\`\``;
"#,
        );
    }

    #[test]
    fn keeps_non_consecutive_concat_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
"the".concat(first, " take the ").concat(second, " and ").split(" ").concat(third);
"#,
            r#"
`the${first} take the ${second} and `.split(" ").concat(third);
"#,
        );
    }
}
