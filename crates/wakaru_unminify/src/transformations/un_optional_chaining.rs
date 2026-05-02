use std::collections::HashSet;

use oxc_allocator::{CloneIn, TakeIn};
use oxc_ast::{
    ast::{
        Argument, AssignmentExpression, AssignmentTarget, BindingPattern, CallExpression,
        ConditionalExpression, Expression, Statement,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::Span;
use oxc_syntax::operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut transformer = OptionalChainingTransformer {
        ast: AstBuilder::new(source.allocator),
        unused_temps: HashSet::new(),
    };

    transformer.visit_program(&mut source.program);

    Ok(())
}

struct OptionalChainingTransformer<'a> {
    ast: AstBuilder<'a>,
    unused_temps: HashSet<String>,
}

impl<'a> VisitMut<'a> for OptionalChainingTransformer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        walk_mut::walk_expression(self, expression);

        if let Some(replacement) = self.convert_conditional_expression(expression) {
            *expression = replacement;
        } else if let Some(replacement) = self.convert_logical_expression(expression) {
            *expression = replacement;
        }
    }

    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        walk_mut::walk_statements(self, statements);
        self.remove_unused_temp_declarations(statements);
    }
}

impl<'a> OptionalChainingTransformer<'a> {
    fn convert_conditional_expression(
        &mut self,
        expression: &Expression<'a>,
    ) -> Option<Expression<'a>> {
        let Expression::ConditionalExpression(conditional) = expression else {
            return None;
        };

        let (temp_name, target) = optional_member_guard(conditional)?;
        let replacement = self.optional_chain_expression(
            conditional.span,
            target,
            temp_name,
            without_parentheses(&conditional.alternate),
        )?;

        self.unused_temps.insert(temp_name.to_string());
        Some(replacement)
    }

    fn convert_logical_expression(
        &mut self,
        expression: &Expression<'a>,
    ) -> Option<Expression<'a>> {
        let Expression::LogicalExpression(logical) = expression else {
            return None;
        };
        if logical.operator != LogicalOperator::Or {
            return None;
        }

        let guard = optional_or_guard(&logical.left)?;
        let replacement = self.optional_chain_expression(
            logical.span,
            guard.target,
            guard.temp_name,
            without_parentheses(&logical.right),
        )?;

        if let Some(temp_name) = guard.unused_temp {
            self.unused_temps.insert(temp_name.to_string());
        }

        Some(replacement)
    }

    fn optional_chain_expression(
        &self,
        span: Span,
        object: &Expression<'a>,
        temp_name: &str,
        member: &Expression<'a>,
    ) -> Option<Expression<'a>> {
        let expression = match without_parentheses(member) {
            Expression::StaticMemberExpression(member) => {
                Expression::StaticMemberExpression(self.ast.alloc_static_member_expression(
                    member.span,
                    object.clone_in(self.ast.allocator),
                    member.property.clone_in(self.ast.allocator),
                    true,
                ))
            }
            Expression::ComputedMemberExpression(member) => {
                Expression::ComputedMemberExpression(self.ast.alloc_computed_member_expression(
                    member.span,
                    object.clone_in(self.ast.allocator),
                    member.expression.clone_in(self.ast.allocator),
                    true,
                ))
            }
            Expression::CallExpression(call) => {
                return self.optional_call_expression(span, object, temp_name, call);
            }
            _ => return None,
        };

        let chain_element = expression.into_chain_element()?;
        Some(self.ast.expression_chain(span, chain_element))
    }

    fn optional_call_expression(
        &self,
        span: Span,
        target: &Expression<'a>,
        temp_name: &str,
        call: &CallExpression<'a>,
    ) -> Option<Expression<'a>> {
        match without_parentheses(&call.callee) {
            Expression::Identifier(identifier) if identifier.name == temp_name => {
                self.optional_call(span, target.clone_in(self.ast.allocator), call, 0, true)
            }
            Expression::StaticMemberExpression(member)
                if identifier_name(&member.object) == Some(temp_name) =>
            {
                if member.property.name == "call" {
                    return self.optional_call_method(span, target, call);
                }

                if matches!(member.property.name.as_str(), "apply" | "bind") {
                    return None;
                }

                let callee =
                    Expression::StaticMemberExpression(self.ast.alloc_static_member_expression(
                        member.span,
                        target.clone_in(self.ast.allocator),
                        member.property.clone_in(self.ast.allocator),
                        true,
                    ));
                self.optional_call(span, callee, call, 0, false)
            }
            Expression::ComputedMemberExpression(member)
                if identifier_name(&member.object) == Some(temp_name) =>
            {
                let callee = Expression::ComputedMemberExpression(
                    self.ast.alloc_computed_member_expression(
                        member.span,
                        target.clone_in(self.ast.allocator),
                        member.expression.clone_in(self.ast.allocator),
                        true,
                    ),
                );
                self.optional_call(span, callee, call, 0, false)
            }
            _ => None,
        }
    }

    fn optional_call_method(
        &self,
        span: Span,
        target: &Expression<'a>,
        call: &CallExpression<'a>,
    ) -> Option<Expression<'a>> {
        let first_argument = call.arguments.first()?;
        let expected_this = call_this_expression(target);
        if !argument_matches_expression(first_argument, expected_this) {
            return None;
        }

        self.optional_call(span, target.clone_in(self.ast.allocator), call, 1, true)
    }

    fn optional_call(
        &self,
        span: Span,
        callee: Expression<'a>,
        call: &CallExpression<'a>,
        skip_arguments: usize,
        optional: bool,
    ) -> Option<Expression<'a>> {
        let mut arguments = self
            .ast
            .vec_with_capacity(call.arguments.len().saturating_sub(skip_arguments));
        for argument in call.arguments.iter().skip(skip_arguments) {
            arguments.push(argument.clone_in(self.ast.allocator));
        }

        let expression = Expression::CallExpression(self.ast.alloc_call_expression_with_pure(
            call.span,
            callee,
            call.type_arguments.clone_in(self.ast.allocator),
            arguments,
            optional,
            call.pure,
        ));
        let chain_element = expression.into_chain_element()?;
        Some(self.ast.expression_chain(span, chain_element))
    }

    fn remove_unused_temp_declarations(
        &self,
        statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        if self.unused_temps.is_empty() {
            return;
        }

        let old_statements = statements.take_in(self.ast);
        let mut new_statements = self.ast.vec_with_capacity(old_statements.len());

        for statement in old_statements {
            if let Some(statement) = self.remove_unused_temp_declaration(statement) {
                new_statements.push(statement);
            }
        }

        *statements = new_statements;
    }

    fn remove_unused_temp_declaration(&self, statement: Statement<'a>) -> Option<Statement<'a>> {
        let Statement::VariableDeclaration(mut declaration) = statement else {
            return Some(statement);
        };

        if !declaration
            .declarations
            .iter()
            .any(|declarator| unused_temp_declarator(declarator, &self.unused_temps))
        {
            return Some(Statement::VariableDeclaration(declaration));
        }

        let old_declarations = declaration.declarations.take_in(self.ast);
        let mut new_declarations = self.ast.vec_with_capacity(old_declarations.len());

        for declarator in old_declarations {
            if !unused_temp_declarator(&declarator, &self.unused_temps) {
                new_declarations.push(declarator);
            }
        }

        if new_declarations.is_empty() {
            None
        } else {
            declaration.declarations = new_declarations;
            Some(Statement::VariableDeclaration(declaration))
        }
    }
}

fn optional_member_guard<'a, 'b>(
    conditional: &'b ConditionalExpression<'a>,
) -> Option<(&'b str, &'b Expression<'a>)> {
    if !is_undefined_expression(without_parentheses(&conditional.consequent)) {
        return None;
    }

    let Expression::LogicalExpression(logical) = without_parentheses(&conditional.test) else {
        return None;
    };
    if logical.operator != LogicalOperator::Or {
        return None;
    }

    let (temp_name, target) = assignment_null_check(without_parentheses(&logical.left))?;

    if !identifier_nullish_check(without_parentheses(&logical.right), temp_name) {
        return None;
    }

    Some((temp_name, target))
}

struct OptionalOrGuard<'a, 'b> {
    temp_name: &'b str,
    target: &'b Expression<'a>,
    unused_temp: Option<&'b str>,
}

fn optional_or_guard<'a, 'b>(expression: &'b Expression<'a>) -> Option<OptionalOrGuard<'a, 'b>> {
    let Expression::LogicalExpression(logical) = without_parentheses(expression) else {
        return None;
    };
    if logical.operator != LogicalOperator::Or {
        return None;
    }

    if let Some((temp_name, target)) = assignment_null_check(without_parentheses(&logical.left)) {
        if identifier_nullish_check(without_parentheses(&logical.right), temp_name) {
            return Some(OptionalOrGuard {
                temp_name,
                target,
                unused_temp: Some(temp_name),
            });
        }
    }

    if let Some((temp_name, target)) = identifier_null_check(without_parentheses(&logical.left)) {
        if identifier_nullish_check(without_parentheses(&logical.right), temp_name) {
            return Some(OptionalOrGuard {
                temp_name,
                target,
                unused_temp: None,
            });
        }
    }

    None
}

fn identifier_null_check<'a, 'b>(
    expression: &'b Expression<'a>,
) -> Option<(&'b str, &'b Expression<'a>)> {
    let Expression::BinaryExpression(binary) = expression else {
        return None;
    };
    if !is_equality_operator(binary.operator) {
        return None;
    }

    if matches!(
        without_parentheses(&binary.right),
        Expression::NullLiteral(_)
    ) {
        let name = identifier_name(&binary.left)?;
        return Some((name, without_parentheses(&binary.left)));
    }

    if matches!(
        without_parentheses(&binary.left),
        Expression::NullLiteral(_)
    ) {
        let name = identifier_name(&binary.right)?;
        return Some((name, without_parentheses(&binary.right)));
    }

    None
}

fn assignment_null_check<'a, 'b>(
    expression: &'b Expression<'a>,
) -> Option<(&'b str, &'b Expression<'a>)> {
    let Expression::BinaryExpression(binary) = expression else {
        return None;
    };
    if !is_equality_operator(binary.operator) {
        return None;
    }

    if let Some(result) = assignment_compared_to_null(&binary.left, &binary.right) {
        return Some(result);
    }
    assignment_compared_to_null(&binary.right, &binary.left)
}

fn assignment_compared_to_null<'a, 'b>(
    maybe_assignment: &'b Expression<'a>,
    maybe_null: &Expression<'a>,
) -> Option<(&'b str, &'b Expression<'a>)> {
    if !matches!(without_parentheses(maybe_null), Expression::NullLiteral(_)) {
        return None;
    }

    let Expression::AssignmentExpression(assignment) = without_parentheses(maybe_assignment) else {
        return None;
    };
    assignment_target(assignment)
}

fn assignment_target<'a, 'b>(
    assignment: &'b AssignmentExpression<'a>,
) -> Option<(&'b str, &'b Expression<'a>)> {
    if assignment.operator != AssignmentOperator::Assign {
        return None;
    }

    let AssignmentTarget::AssignmentTargetIdentifier(identifier) = &assignment.left else {
        return None;
    };

    Some((identifier.name.as_str(), &assignment.right))
}

fn identifier_nullish_check(expression: &Expression, name: &str) -> bool {
    let Expression::BinaryExpression(binary) = expression else {
        return false;
    };
    if !is_equality_operator(binary.operator) {
        return false;
    }

    (identifier_name(&binary.left) == Some(name) && is_undefined_expression(&binary.right))
        || (identifier_name(&binary.right) == Some(name) && is_undefined_expression(&binary.left))
}

fn identifier_name<'a>(expression: &'a Expression) -> Option<&'a str> {
    let Expression::Identifier(identifier) = without_parentheses(expression) else {
        return None;
    };

    Some(identifier.name.as_str())
}

fn is_equality_operator(operator: BinaryOperator) -> bool {
    matches!(
        operator,
        BinaryOperator::Equality | BinaryOperator::StrictEquality
    )
}

fn call_this_expression<'a, 'b>(target: &'b Expression<'a>) -> &'b Expression<'a> {
    match without_parentheses(target) {
        Expression::StaticMemberExpression(member) => without_parentheses(&member.object),
        Expression::ComputedMemberExpression(member) => without_parentheses(&member.object),
        _ => without_parentheses(target),
    }
}

fn argument_matches_expression(argument: &Argument, expression: &Expression) -> bool {
    let Some(argument) = argument.as_expression() else {
        return false;
    };

    expressions_match(argument, expression)
}

fn expressions_match(left: &Expression, right: &Expression) -> bool {
    match (without_parentheses(left), without_parentheses(right)) {
        (Expression::Identifier(left), Expression::Identifier(right)) => left.name == right.name,
        (Expression::ThisExpression(_), Expression::ThisExpression(_)) => true,
        (Expression::StaticMemberExpression(left), Expression::StaticMemberExpression(right)) => {
            left.property.name == right.property.name
                && expressions_match(&left.object, &right.object)
        }
        (
            Expression::ComputedMemberExpression(left),
            Expression::ComputedMemberExpression(right),
        ) => {
            expressions_match(&left.object, &right.object)
                && expressions_match(&left.expression, &right.expression)
        }
        _ => false,
    }
}

fn is_undefined_expression(expression: &Expression) -> bool {
    match without_parentheses(expression) {
        Expression::Identifier(identifier) => identifier.name == "undefined",
        Expression::UnaryExpression(unary) => {
            unary.operator == UnaryOperator::Void && is_numeric_zero(&unary.argument)
        }
        _ => false,
    }
}

fn is_numeric_zero(expression: &Expression) -> bool {
    match without_parentheses(expression) {
        Expression::NumericLiteral(literal) => literal.value == 0.0,
        _ => false,
    }
}

fn unused_temp_declarator(
    declarator: &oxc_ast::ast::VariableDeclarator,
    unused_temps: &HashSet<String>,
) -> bool {
    if declarator.init.is_some() {
        return false;
    }

    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return false;
    };

    unused_temps.contains(identifier.name.as_str())
}

fn without_parentheses<'a, 'b>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            without_parentheses(&parenthesized.expression)
        }
        _ => expression,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn restores_babel_swc_member_access() {
        define_ast_inline_test(transform_ast)(
            "
var _a;
(_a = a) === null || _a === void 0 ? void 0 : _a.b;
",
            "
a?.b;
",
        );
    }

    #[test]
    fn restores_computed_member_access() {
        define_ast_inline_test(transform_ast)(
            "
var _a;
(_a = a) === null || _a === void 0 ? void 0 : _a[0];
",
            "
a?.[0];
",
        );
    }

    #[test]
    fn restores_optional_function_call() {
        define_ast_inline_test(transform_ast)(
            "
var _foo;
(_foo = foo) === null || _foo === void 0 ? void 0 : _foo(bar);
",
            "
foo?.(bar);
",
        );
    }

    #[test]
    fn restores_optional_member_call() {
        define_ast_inline_test(transform_ast)(
            "
var _foo;
(_foo = foo) === null || _foo === void 0 ? void 0 : _foo.bar(baz);
",
            "
foo?.bar(baz);
",
        );
    }

    #[test]
    fn restores_logical_or_member_access() {
        define_ast_inline_test(transform_ast)(
            "
var _foo;
(_foo = foo) === null || _foo === void 0 || _foo.bar;

foo === null || foo === void 0 || foo.baz;
",
            "
foo?.bar;
foo?.baz;
",
        );
    }

    #[test]
    fn restores_logical_or_calls() {
        define_ast_inline_test(transform_ast)(
            "
var _foo, _bar;
(_foo = foo) === null || _foo === void 0 || _foo(bar);
(_bar = foo.bar) === null || _bar === void 0 || _bar.call(foo, baz);
",
            "
foo?.(bar);
foo.bar?.(baz);
",
        );
    }

    #[test]
    fn restores_call_method_optional_call() {
        define_ast_inline_test(transform_ast)(
            "
var _foo_bar;
(_foo_bar = foo.bar) === null || _foo_bar === void 0 ? void 0 : _foo_bar.call(foo, baz);
",
            "
foo.bar?.(baz);
",
        );
    }

    #[test]
    fn leaves_mismatched_temp_guards_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
var _a;
(_a = a) === null || _b === void 0 ? void 0 : _a.b;
",
            "
var _a;
(_a = a) === null || _b === void 0 ? void 0 : _a.b;
",
        );
    }
}
