use oxc_allocator::TakeIn;
use oxc_ast::{ast::Expression, AstBuilder};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_parser::Kind;
use oxc_syntax::{identifier::is_identifier_name, number::NumberBase};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut normalizer = BracketNotationNormalizer {
        ast: AstBuilder::new(source.allocator),
    };

    normalizer.visit_program(&mut source.program);

    Ok(())
}

struct BracketNotationNormalizer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for BracketNotationNormalizer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        walk_mut::walk_expression(self, expression);
        self.normalize_computed_member(expression);
    }
}

impl<'a> BracketNotationNormalizer<'a> {
    fn normalize_computed_member(&mut self, expression: &mut Expression<'a>) {
        let Expression::ComputedMemberExpression(member) = expression else {
            return;
        };

        let Expression::StringLiteral(property) = &member.expression else {
            return;
        };

        let value = property.value.as_str().to_string();
        let property_span = property.span;

        if let Some(number) = numeric_property_value(&value) {
            member.expression = self.ast.expression_numeric_literal(
                property_span,
                number,
                None,
                NumberBase::Decimal,
            );
            return;
        }

        if !is_valid_member_identifier(&value) {
            return;
        }

        let span = member.span;
        let optional = member.optional;
        let object = member.object.take_in(self.ast);
        let arena_name = self.ast.allocator.alloc_str(&value);
        let property = self.ast.identifier_name(property_span, arena_name);

        *expression = Expression::StaticMemberExpression(
            self.ast
                .alloc_static_member_expression(span, object, property, optional),
        );
    }
}

fn numeric_property_value(value: &str) -> Option<f64> {
    if !is_decimal_property_text(value) {
        return None;
    }

    let number = value.parse::<f64>().ok()?;
    if number.to_string() == value {
        Some(number)
    } else {
        None
    }
}

fn is_decimal_property_text(value: &str) -> bool {
    let Some((head, tail)) = value.split_once('.') else {
        return !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit());
    };

    !head.is_empty()
        && !tail.is_empty()
        && head.chars().all(|ch| ch.is_ascii_digit())
        && tail.chars().all(|ch| ch.is_ascii_digit())
}

fn is_valid_member_identifier(value: &str) -> bool {
    if !is_identifier_name(value) {
        return false;
    }

    let kind = Kind::match_keyword(value);
    !kind.is_reserved_keyword() && !kind.is_strict_mode_contextual_keyword()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn transforms_string_properties_to_dot_notation() {
        define_ast_inline_test(transform_ast)(
            "
obj['bar'];
obj['bar'].baz;
obj['bar']['baz'];
obj['bar'].baz['qux'];
obj['\u{0EB3}'];
obj['\u{001B}'];
",
            "
obj.bar;
obj.bar.baz;
obj.bar.baz;
obj.bar.baz.qux;
obj.ຳ;
obj[\"\\x1B\"];
",
        );
    }

    #[test]
    fn transforms_numeric_string_properties_to_numeric_members() {
        define_ast_inline_test(transform_ast)(
            "
obj['1'];
obj['0'];
obj['00'];
obj['-0'];
obj['-1'];
obj['1_1'];

obj['3.14'];
obj['3.14e-10'];
obj['3.'];
obj['3..7'];
",
            "
obj[1];
obj[0];
obj[\"00\"];
obj[\"-0\"];
obj[\"-1\"];
obj[\"1_1\"];
obj[3.14];
obj[\"3.14e-10\"];
obj[\"3.\"];
obj[\"3..7\"];
",
        );
    }

    #[test]
    fn leaves_invalid_or_reserved_properties_in_brackets() {
        define_ast_inline_test(transform_ast)(
            "
obj[a];
obj[''];
obj[' '];
obj['var'];
obj['let'];
obj['const'];
obj['await'];
obj['1var'];
obj['prop-with-dash'];
obj['get'];
",
            "
obj[a];
obj[\"\"];
obj[\" \"];
obj[\"var\"];
obj[\"let\"];
obj[\"const\"];
obj[\"await\"];
obj[\"1var\"];
obj[\"prop-with-dash\"];
obj.get;
",
        );
    }
}
