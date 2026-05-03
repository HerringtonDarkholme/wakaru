use std::collections::HashSet;

use oxc_allocator::{CloneIn, TakeIn};
use oxc_ast::{
    ast::{
        Argument, ArrayExpressionElement, AssignmentExpression, AssignmentTarget, BindingPattern,
        CallExpression, ConditionalExpression, Expression, Statement,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::{GetSpan, Span};
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

        let guard =
            optional_member_guard(conditional).or_else(|| optional_delete_guard(conditional))?;
        let replacement = self.optional_chain_expression(
            conditional.span,
            guard.target,
            guard.temp_name,
            without_parentheses(&conditional.alternate),
        )?;

        if let Some(temp_name) = guard.unused_temp {
            self.unused_temps.insert(temp_name.to_string());
        }
        Some(replacement)
    }

    fn convert_logical_expression(
        &mut self,
        expression: &Expression<'a>,
    ) -> Option<Expression<'a>> {
        let Expression::LogicalExpression(logical) = expression else {
            return None;
        };

        let guard = match logical.operator {
            LogicalOperator::Or => optional_or_guard(&logical.left)?,
            LogicalOperator::And => optional_and_guard(&logical.left)?,
            _ => return None,
        };
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
            Expression::StaticMemberExpression(_) | Expression::ComputedMemberExpression(_) => {
                self.optional_member_access(object, temp_name, member)?
            }
            Expression::CallExpression(call) => {
                return self.optional_call_expression(span, object, temp_name, call);
            }
            Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::Delete => {
                let argument = self.optional_chain_expression(
                    unary.span,
                    object,
                    temp_name,
                    without_parentheses(&unary.argument),
                )?;
                return Some(self.ast.expression_unary(
                    unary.span,
                    UnaryOperator::Delete,
                    argument,
                ));
            }
            _ => return None,
        };

        let chain_element = expression.into_chain_element()?;
        Some(self.ast.expression_chain(span, chain_element))
    }

    fn optional_member_access(
        &self,
        target: &Expression<'a>,
        temp_name: &str,
        member: &Expression<'a>,
    ) -> Option<Expression<'a>> {
        match without_parentheses(member) {
            Expression::StaticMemberExpression(member) => {
                let (object, optional) =
                    self.optional_member_object(target, temp_name, &member.object)?;
                Some(Expression::StaticMemberExpression(
                    self.ast.alloc_static_member_expression(
                        member.span,
                        object,
                        member.property.clone_in(self.ast.allocator),
                        optional,
                    ),
                ))
            }
            Expression::ComputedMemberExpression(member) => {
                let (object, optional) =
                    self.optional_member_object(target, temp_name, &member.object)?;
                Some(Expression::ComputedMemberExpression(
                    self.ast.alloc_computed_member_expression(
                        member.span,
                        object,
                        member.expression.clone_in(self.ast.allocator),
                        optional,
                    ),
                ))
            }
            _ => None,
        }
    }

    fn optional_member_object(
        &self,
        target: &Expression<'a>,
        temp_name: &str,
        object: &Expression<'a>,
    ) -> Option<(Expression<'a>, bool)> {
        if identifier_name(object) == Some(temp_name) || expressions_match(object, target) {
            return Some((target.clone_in(self.ast.allocator), true));
        }

        let object = self.optional_member_access(target, temp_name, object)?;
        Some((object, false))
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

                if member.property.name == "apply" {
                    return self.optional_apply_call(
                        span,
                        target.clone_in(self.ast.allocator),
                        call,
                    );
                }

                if member.property.name == "bind" {
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
            Expression::StaticMemberExpression(apply_member)
                if apply_member.property.name == "apply" =>
            {
                self.optional_apply_member_call(span, target, temp_name, apply_member, call)
            }
            Expression::StaticMemberExpression(bind_member)
                if bind_member.property.name == "bind" =>
            {
                self.optional_bind_member_expression(span, target, temp_name, bind_member, call)
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

    fn optional_apply_member_call(
        &self,
        span: Span,
        target: &Expression<'a>,
        temp_name: &str,
        apply_member: &oxc_ast::ast::StaticMemberExpression<'a>,
        call: &CallExpression<'a>,
    ) -> Option<Expression<'a>> {
        match without_parentheses(&apply_member.object) {
            Expression::StaticMemberExpression(member)
                if identifier_name(&member.object) == Some(temp_name) =>
            {
                let expected_this = call_this_expression(target);
                if !call.arguments.first().is_some_and(|argument| {
                    argument_matches_expression(argument, expected_this)
                        || argument_matches_identifier(argument, temp_name)
                }) {
                    return None;
                }

                let callee =
                    Expression::StaticMemberExpression(self.ast.alloc_static_member_expression(
                        member.span,
                        target.clone_in(self.ast.allocator),
                        member.property.clone_in(self.ast.allocator),
                        true,
                    ));
                self.optional_apply_call(span, callee, call)
            }
            Expression::ComputedMemberExpression(member)
                if identifier_name(&member.object) == Some(temp_name) =>
            {
                let expected_this = call_this_expression(target);
                if !call.arguments.first().is_some_and(|argument| {
                    argument_matches_expression(argument, expected_this)
                        || argument_matches_identifier(argument, temp_name)
                }) {
                    return None;
                }

                let callee = Expression::ComputedMemberExpression(
                    self.ast.alloc_computed_member_expression(
                        member.span,
                        target.clone_in(self.ast.allocator),
                        member.expression.clone_in(self.ast.allocator),
                        true,
                    ),
                );
                self.optional_apply_call(span, callee, call)
            }
            _ => None,
        }
    }

    fn optional_apply_call(
        &self,
        span: Span,
        callee: Expression<'a>,
        call: &CallExpression<'a>,
    ) -> Option<Expression<'a>> {
        let argument = call.arguments.get(1)?;
        if matches!(argument, Argument::SpreadElement(_)) {
            return None;
        }

        let arguments = self.apply_arguments(argument);
        let expression = Expression::CallExpression(self.ast.alloc_call_expression_with_pure(
            call.span,
            callee,
            call.type_arguments.clone_in(self.ast.allocator),
            arguments,
            true,
            call.pure,
        ));
        let chain_element = expression.into_chain_element()?;
        Some(self.ast.expression_chain(span, chain_element))
    }

    fn apply_arguments(&self, argument: &Argument<'a>) -> oxc_allocator::Vec<'a, Argument<'a>> {
        if let Some(Expression::ArrayExpression(array)) = argument.as_expression() {
            let mut arguments = self.ast.vec_with_capacity(array.elements.len());
            for element in &array.elements {
                arguments.push(array_element_to_argument(self.ast, element));
            }
            return arguments;
        }

        let mut arguments = self.ast.vec_with_capacity(1);
        let Some(expression) = argument.as_expression() else {
            return arguments;
        };
        arguments.push(
            self.ast
                .argument_spread_element(argument.span(), expression.clone_in(self.ast.allocator)),
        );
        arguments
    }

    fn optional_bind_member_expression(
        &self,
        span: Span,
        target: &Expression<'a>,
        temp_name: &str,
        bind_member: &oxc_ast::ast::StaticMemberExpression<'a>,
        call: &CallExpression<'a>,
    ) -> Option<Expression<'a>> {
        let first_argument = call.arguments.first()?;
        let expected_this = call_this_expression(target);
        if !argument_matches_expression(first_argument, expected_this)
            && !argument_matches_identifier(first_argument, temp_name)
        {
            return None;
        }

        let optional_member = match without_parentheses(&bind_member.object) {
            Expression::StaticMemberExpression(member)
                if identifier_name(&member.object) == Some(temp_name) =>
            {
                Expression::StaticMemberExpression(self.ast.alloc_static_member_expression(
                    member.span,
                    target.clone_in(self.ast.allocator),
                    member.property.clone_in(self.ast.allocator),
                    true,
                ))
            }
            Expression::ComputedMemberExpression(member)
                if identifier_name(&member.object) == Some(temp_name) =>
            {
                Expression::ComputedMemberExpression(self.ast.alloc_computed_member_expression(
                    member.span,
                    target.clone_in(self.ast.allocator),
                    member.expression.clone_in(self.ast.allocator),
                    true,
                ))
            }
            _ => return None,
        };

        let chain_element = optional_member.into_chain_element()?;
        Some(self.ast.expression_chain(span, chain_element))
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
) -> Option<OptionalOrGuard<'a, 'b>> {
    if !is_undefined_expression(without_parentheses(&conditional.consequent)) {
        return None;
    }

    let Expression::LogicalExpression(logical) = without_parentheses(&conditional.test) else {
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

fn optional_delete_guard<'a, 'b>(
    conditional: &'b ConditionalExpression<'a>,
) -> Option<OptionalOrGuard<'a, 'b>> {
    if !is_true_expression(without_parentheses(&conditional.consequent)) {
        return None;
    }

    let Expression::UnaryExpression(unary) = without_parentheses(&conditional.alternate) else {
        return None;
    };
    if unary.operator != UnaryOperator::Delete {
        return None;
    }

    let Expression::LogicalExpression(logical) = without_parentheses(&conditional.test) else {
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

fn optional_and_guard<'a, 'b>(expression: &'b Expression<'a>) -> Option<OptionalOrGuard<'a, 'b>> {
    let Expression::LogicalExpression(logical) = without_parentheses(expression) else {
        return None;
    };
    if logical.operator != LogicalOperator::And {
        return None;
    }

    if let Some((temp_name, target)) = assignment_not_null_check(without_parentheses(&logical.left))
    {
        if identifier_not_nullish_check(without_parentheses(&logical.right), temp_name) {
            return Some(OptionalOrGuard {
                temp_name,
                target,
                unused_temp: Some(temp_name),
            });
        }
    }

    if let Some((temp_name, target)) = identifier_not_null_check(without_parentheses(&logical.left))
    {
        if identifier_not_nullish_check(without_parentheses(&logical.right), temp_name) {
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

fn identifier_not_null_check<'a, 'b>(
    expression: &'b Expression<'a>,
) -> Option<(&'b str, &'b Expression<'a>)> {
    let Expression::BinaryExpression(binary) = expression else {
        return None;
    };
    if !is_inequality_operator(binary.operator) {
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

fn array_element_to_argument<'a>(
    ast: AstBuilder<'a>,
    element: &ArrayExpressionElement<'a>,
) -> Argument<'a> {
    match element {
        ArrayExpressionElement::SpreadElement(spread) => {
            Argument::SpreadElement(spread.clone_in(ast.allocator))
        }
        ArrayExpressionElement::Elision(elision) => {
            expression_to_argument(ast.expression_identifier(elision.span, ast.ident("undefined")))
        }
        ArrayExpressionElement::BooleanLiteral(value) => {
            Argument::BooleanLiteral(value.clone_in(ast.allocator))
        }
        ArrayExpressionElement::NullLiteral(value) => {
            Argument::NullLiteral(value.clone_in(ast.allocator))
        }
        ArrayExpressionElement::NumericLiteral(value) => {
            Argument::NumericLiteral(value.clone_in(ast.allocator))
        }
        ArrayExpressionElement::StringLiteral(value) => {
            Argument::StringLiteral(value.clone_in(ast.allocator))
        }
        ArrayExpressionElement::Identifier(value) => {
            Argument::Identifier(value.clone_in(ast.allocator))
        }
        ArrayExpressionElement::ThisExpression(value) => {
            Argument::ThisExpression(value.clone_in(ast.allocator))
        }
        ArrayExpressionElement::StaticMemberExpression(value) => {
            Argument::StaticMemberExpression(value.clone_in(ast.allocator))
        }
        ArrayExpressionElement::ComputedMemberExpression(value) => {
            Argument::ComputedMemberExpression(value.clone_in(ast.allocator))
        }
        ArrayExpressionElement::CallExpression(value) => {
            Argument::CallExpression(value.clone_in(ast.allocator))
        }
        _ => expression_to_argument(
            ast.expression_identifier(element.span(), ast.ident("undefined")),
        ),
    }
}

fn expression_to_argument<'a>(expression: Expression<'a>) -> Argument<'a> {
    match expression {
        Expression::BooleanLiteral(value) => Argument::BooleanLiteral(value),
        Expression::NullLiteral(value) => Argument::NullLiteral(value),
        Expression::NumericLiteral(value) => Argument::NumericLiteral(value),
        Expression::StringLiteral(value) => Argument::StringLiteral(value),
        Expression::Identifier(value) => Argument::Identifier(value),
        Expression::ThisExpression(value) => Argument::ThisExpression(value),
        Expression::StaticMemberExpression(value) => Argument::StaticMemberExpression(value),
        Expression::ComputedMemberExpression(value) => Argument::ComputedMemberExpression(value),
        Expression::CallExpression(value) => Argument::CallExpression(value),
        _ => unreachable!("only supported expression variants are converted to arguments"),
    }
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

fn assignment_not_null_check<'a, 'b>(
    expression: &'b Expression<'a>,
) -> Option<(&'b str, &'b Expression<'a>)> {
    let Expression::BinaryExpression(binary) = expression else {
        return None;
    };
    if !is_inequality_operator(binary.operator) {
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

fn identifier_not_nullish_check(expression: &Expression, name: &str) -> bool {
    let Expression::BinaryExpression(binary) = expression else {
        return false;
    };
    if !is_inequality_operator(binary.operator) {
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

fn is_inequality_operator(operator: BinaryOperator) -> bool {
    matches!(
        operator,
        BinaryOperator::Inequality | BinaryOperator::StrictInequality
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

fn argument_matches_identifier(argument: &Argument, name: &str) -> bool {
    let Some(Expression::Identifier(identifier)) = argument.as_expression() else {
        return false;
    };

    identifier.name == name
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

fn is_true_expression(expression: &Expression) -> bool {
    matches!(
        without_parentheses(expression),
        Expression::BooleanLiteral(literal) if literal.value
    )
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
    fn restores_logical_and_truthy_guards() {
        define_ast_inline_test(transform_ast)(
            "
var _foo, _bar, _baz;
foo !== null && foo !== void 0 && foo.bar;
(_foo = foo) !== null && _foo !== void 0 && _foo.bar;
(_bar = foo) !== null && _bar !== void 0 && _bar(baz);
(_baz = foo.bar) !== null && _baz !== void 0 && _baz.call(foo, baz);
",
            "
foo?.bar;
foo?.bar;
foo?.(baz);
foo.bar?.(baz);
",
        );
    }

    #[test]
    fn restores_delete_optional_chaining() {
        define_ast_inline_test(transform_ast)(
            "
var _obj, _foo;
obj === null || obj === void 0 || delete obj.a;
obj === null || obj === void 0 || delete obj.a.b;
(_obj = obj) === null || _obj === void 0 ? true : delete _obj.a;
(_foo = foo) === null || _foo === void 0 ? true : delete _foo.bar();
",
            "
delete obj?.a;
delete obj?.a.b;
delete obj?.a;
delete foo?.bar();
",
        );
    }

    #[test]
    fn restores_optional_chaining_in_containers() {
        define_ast_inline_test(transform_ast)(
            "
var _user$address, _user$address2, _a, _a2, _a3;
var street = (_user$address = user.address) === null || _user$address === void 0 ? void 0 : _user$address.street;
street = (_user$address2 = user.address) === null || _user$address2 === void 0 ? void 0 : _user$address2.street;
test((_a = a) === null || _a === void 0 ? void 0 : _a.b, 1);
test((_a2 = a) === null || _a2 === void 0 ? void 0 : _a2.b, 1);
1, (_a3 = a) !== null && _a3 !== void 0 && _a3.b, 2;
",
            "
var street = user.address?.street;
street = user.address?.street;
test(a?.b, 1);
test(a?.b, 1);
1, a?.b, 2;
",
        );
    }

    #[test]
    fn restores_apply_optional_calls() {
        define_ast_inline_test(transform_ast)(
            "
var _foo, _bar, _baz;
(_foo = foo) === null || _foo === void 0 ? void 0 : _foo.apply(void 0, args);
(_bar = foo.bar) === null || _bar === void 0 ? void 0 : _bar.apply(foo, [baz, qux]);
(_baz = foo) === null || _baz === void 0 || _baz.bar.apply(_baz, args);
",
            "
foo?.(...args);
foo.bar?.(baz, qux);
foo?.bar?.(...args);
",
        );
    }

    #[test]
    fn restores_bind_optional_member_expressions() {
        define_ast_inline_test(transform_ast)(
            "
var _foo, _bar;
((_foo = foo) === null || _foo === void 0 ? void 0 : _foo.m.bind(_foo))();
((_bar = foo) === null || _bar === void 0 ? void 0 : _bar[method].bind(_bar))();
(Foo === null || Foo === void 0 ? void 0 : Foo[\"m\"].bind(Foo))();
",
            "
(foo?.m)();
(foo?.[method])();
(Foo?.[\"m\"])();
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
