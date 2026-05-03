use std::collections::HashSet;

use oxc_allocator::{CloneIn, TakeIn};
use oxc_ast::{
    ast::{
        AssignmentExpression, AssignmentTarget, BindingPattern, ConditionalExpression, Expression,
        Statement,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_syntax::operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut transformer = NullishCoalescingTransformer {
        ast: AstBuilder::new(source.allocator),
        unused_temps: HashSet::new(),
    };

    transformer.visit_program(&mut source.program);

    Ok(())
}

struct NullishCoalescingTransformer<'a> {
    ast: AstBuilder<'a>,
    unused_temps: HashSet<String>,
}

impl<'a> VisitMut<'a> for NullishCoalescingTransformer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        walk_mut::walk_expression(self, expression);

        if let Some(replacement) = self.convert_conditional_expression(expression) {
            *expression = replacement;
            return;
        }

        if let Some(replacement) = self.convert_logical_expression(expression) {
            *expression = replacement;
        }
    }

    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        walk_mut::walk_statements(self, statements);
        self.remove_unused_temp_declarations(statements);
    }
}

impl<'a> NullishCoalescingTransformer<'a> {
    fn convert_conditional_expression(
        &mut self,
        expression: &Expression<'a>,
    ) -> Option<Expression<'a>> {
        let Expression::ConditionalExpression(conditional) = expression else {
            return None;
        };

        if let Some(guard) = nullish_guard(conditional) {
            let target = self.guard_target(guard);
            return Some(self.ast.expression_logical(
                conditional.span,
                target.clone_in(self.ast.allocator),
                LogicalOperator::Coalesce,
                conditional.alternate.clone_in(self.ast.allocator),
            ));
        }

        Some(
            self.ast.expression_logical(
                conditional.span,
                self.guard_target(negated_nullish_guard(conditional)?)
                    .clone_in(self.ast.allocator),
                LogicalOperator::Coalesce,
                conditional.consequent.clone_in(self.ast.allocator),
            ),
        )
    }

    fn convert_logical_expression(
        &mut self,
        expression: &Expression<'a>,
    ) -> Option<Expression<'a>> {
        let Expression::LogicalExpression(logical) = expression else {
            return None;
        };
        if logical.operator != LogicalOperator::And {
            return None;
        }

        let guard = nullish_test_guard(&logical.left, &logical.right)?;
        let target = match guard {
            NullishGuard::Direct(target) => target,
            NullishGuard::Temp { name, target } => {
                self.unused_temps.insert(name.to_string());
                target
            }
        };

        Some(self.ast.expression_logical(
            logical.span,
            target.clone_in(self.ast.allocator),
            LogicalOperator::Coalesce,
            self.ast.expression_boolean_literal(logical.span, false),
        ))
    }

    fn guard_target<'b>(&mut self, guard: NullishGuard<'a, 'b>) -> &'b Expression<'a> {
        match guard {
            NullishGuard::Direct(target) => target,
            NullishGuard::Temp { name, target } => {
                self.unused_temps.insert(name.to_string());
                target
            }
        }
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

enum NullishGuard<'a, 'b> {
    Direct(&'b Expression<'a>),
    Temp {
        name: &'b str,
        target: &'b Expression<'a>,
    },
}

fn nullish_guard<'a, 'b>(
    conditional: &'b ConditionalExpression<'a>,
) -> Option<NullishGuard<'a, 'b>> {
    nullish_test_guard(&conditional.test, &conditional.consequent)
}

fn negated_nullish_guard<'a, 'b>(
    conditional: &'b ConditionalExpression<'a>,
) -> Option<NullishGuard<'a, 'b>> {
    negated_nullish_test_guard(&conditional.test, &conditional.alternate)
}

fn nullish_test_guard<'a, 'b>(
    test: &'b Expression<'a>,
    consequent: &'b Expression<'a>,
) -> Option<NullishGuard<'a, 'b>> {
    let Expression::LogicalExpression(logical) = without_parentheses(test) else {
        return None;
    };
    if logical.operator != LogicalOperator::And {
        return None;
    }

    let null_checked = non_null_check(without_parentheses(&logical.left))?;
    let undefined_checked = non_undefined_check(without_parentheses(&logical.right))?;

    match (null_checked, undefined_checked) {
        (CheckedExpression::Direct(left_target), CheckedExpression::Direct(right_target))
            if expressions_equal(left_target, right_target)
                && expressions_equal(left_target, consequent) =>
        {
            Some(NullishGuard::Direct(left_target))
        }
        (CheckedExpression::Temp { name, target }, CheckedExpression::Direct(right_target))
            if identifier_name(right_target) == Some(name)
                && identifier_name(consequent) == Some(name) =>
        {
            Some(NullishGuard::Temp { name, target })
        }
        _ => None,
    }
}

fn negated_nullish_test_guard<'a, 'b>(
    test: &'b Expression<'a>,
    alternate: &'b Expression<'a>,
) -> Option<NullishGuard<'a, 'b>> {
    let Expression::LogicalExpression(logical) = without_parentheses(test) else {
        return None;
    };
    if logical.operator != LogicalOperator::Or {
        return None;
    }

    let null_checked = null_check(without_parentheses(&logical.left))?;
    let undefined_checked = undefined_check(without_parentheses(&logical.right))?;

    match (null_checked, undefined_checked) {
        (CheckedExpression::Direct(left_target), CheckedExpression::Direct(right_target))
            if expressions_equal(left_target, right_target)
                && expressions_equal(left_target, alternate) =>
        {
            Some(NullishGuard::Direct(left_target))
        }
        (CheckedExpression::Temp { name, target }, CheckedExpression::Direct(right_target))
            if identifier_name(right_target) == Some(name)
                && identifier_name(alternate) == Some(name) =>
        {
            Some(NullishGuard::Temp { name, target })
        }
        _ => None,
    }
}

enum CheckedExpression<'a, 'b> {
    Direct(&'b Expression<'a>),
    Temp {
        name: &'b str,
        target: &'b Expression<'a>,
    },
}

fn non_null_check<'a, 'b>(expression: &'b Expression<'a>) -> Option<CheckedExpression<'a, 'b>> {
    let Expression::BinaryExpression(binary) = expression else {
        return None;
    };
    if !is_inequality_operator(binary.operator) {
        return None;
    }

    if let Some(result) = compared_to_null(&binary.left, &binary.right) {
        return Some(result);
    }
    compared_to_null(&binary.right, &binary.left)
}

fn null_check<'a, 'b>(expression: &'b Expression<'a>) -> Option<CheckedExpression<'a, 'b>> {
    let Expression::BinaryExpression(binary) = expression else {
        return None;
    };
    if !is_equality_operator(binary.operator) {
        return None;
    }

    if let Some(result) = compared_to_null(&binary.left, &binary.right) {
        return Some(result);
    }
    compared_to_null(&binary.right, &binary.left)
}

fn compared_to_null<'a, 'b>(
    candidate: &'b Expression<'a>,
    maybe_null: &Expression<'a>,
) -> Option<CheckedExpression<'a, 'b>> {
    if !matches!(without_parentheses(maybe_null), Expression::NullLiteral(_)) {
        return None;
    }

    if let Expression::AssignmentExpression(assignment) = without_parentheses(candidate) {
        let (name, target) = assignment_target(assignment)?;
        return Some(CheckedExpression::Temp { name, target });
    }

    Some(CheckedExpression::Direct(candidate))
}

fn non_undefined_check<'a, 'b>(
    expression: &'b Expression<'a>,
) -> Option<CheckedExpression<'a, 'b>> {
    let Expression::BinaryExpression(binary) = expression else {
        return None;
    };
    if !is_inequality_operator(binary.operator) {
        return None;
    }

    if is_undefined_expression(&binary.right) {
        return Some(CheckedExpression::Direct(&binary.left));
    }
    if is_undefined_expression(&binary.left) {
        return Some(CheckedExpression::Direct(&binary.right));
    }

    None
}

fn undefined_check<'a, 'b>(expression: &'b Expression<'a>) -> Option<CheckedExpression<'a, 'b>> {
    let Expression::BinaryExpression(binary) = expression else {
        return None;
    };
    if !is_equality_operator(binary.operator) {
        return None;
    }

    if is_undefined_expression(&binary.right) {
        return Some(CheckedExpression::Direct(&binary.left));
    }
    if is_undefined_expression(&binary.left) {
        return Some(CheckedExpression::Direct(&binary.right));
    }

    None
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

fn expressions_equal(left: &Expression, right: &Expression) -> bool {
    match (without_parentheses(left), without_parentheses(right)) {
        (Expression::Identifier(left), Expression::Identifier(right)) => left.name == right.name,
        (Expression::ThisExpression(_), Expression::ThisExpression(_)) => true,
        (Expression::StaticMemberExpression(left), Expression::StaticMemberExpression(right)) => {
            left.property.name == right.property.name
                && expressions_equal(&left.object, &right.object)
        }
        (
            Expression::ComputedMemberExpression(left),
            Expression::ComputedMemberExpression(right),
        ) => {
            expressions_equal(&left.object, &right.object)
                && expressions_equal(&left.expression, &right.expression)
        }
        (Expression::StringLiteral(left), Expression::StringLiteral(right)) => {
            left.value == right.value
        }
        (Expression::NumericLiteral(left), Expression::NumericLiteral(right)) => {
            left.value == right.value
        }
        _ => false,
    }
}

fn identifier_name<'a>(expression: &'a Expression) -> Option<&'a str> {
    let Expression::Identifier(identifier) = without_parentheses(expression) else {
        return None;
    };

    Some(identifier.name.as_str())
}

fn is_inequality_operator(operator: BinaryOperator) -> bool {
    matches!(
        operator,
        BinaryOperator::Inequality | BinaryOperator::StrictInequality
    )
}

fn is_equality_operator(operator: BinaryOperator) -> bool {
    matches!(
        operator,
        BinaryOperator::Equality | BinaryOperator::StrictEquality
    )
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
    fn restores_direct_nullish_coalescing() {
        define_ast_inline_test(transform_ast)(
            "
foo !== null && foo !== void 0 ? foo : \"bar\";
foo === null || foo === void 0 ? \"bar\" : foo;
",
            "
foo ?? \"bar\";
foo ?? \"bar\";
",
        );
    }

    #[test]
    fn restores_temp_assignment_nullish_coalescing() {
        define_ast_inline_test(transform_ast)(
            "
var _ref, _foo;
(_ref = foo.bar) !== null && _ref !== void 0 ? _ref : \"qux\";
(_foo = foo.bar) === null || _foo === void 0 ? \"qux\" : _foo;
",
            "
foo.bar ?? \"qux\";
foo.bar ?? \"qux\";
",
        );
    }

    #[test]
    fn restores_nested_nullish_coalescing() {
        define_ast_inline_test(transform_ast)(
            "
var _ref, _foo;
(_ref = foo !== null && foo !== void 0 ? foo : bar) !== null && _ref !== void 0 ? _ref : \"quz\";
(_foo = foo === null || foo === void 0 ? bar : foo) === null || _foo === void 0 ? \"quz\" : _foo;
",
            "
foo ?? bar ?? \"quz\";
foo ?? bar ?? \"quz\";
",
        );
    }

    #[test]
    fn supports_reversed_inequality_operands() {
        define_ast_inline_test(transform_ast)(
            "
var e;
null !== (e = m.foo) && void 0 !== e ? e : void 0;
",
            "
m.foo ?? void 0;
",
        );
    }

    #[test]
    fn restores_logical_leaf_nullish_coalescing() {
        define_ast_inline_test(transform_ast)(
            "
foo !== null && foo !== void 0 && foo;
",
            "
foo ?? false;
",
        );
    }

    #[test]
    fn restores_temp_assignment_logical_leaf_nullish_coalescing() {
        define_ast_inline_test(transform_ast)(
            "
var e;
null !== (e = l.foo.bar) && void 0 !== e && e;
",
            "
l.foo.bar ?? false;
",
        );
    }

    #[test]
    fn restores_nullish_coalescing_after_optional_chaining() {
        define_ast_inline_test(transform_ast)(
            "
var o;
null !== (o = c.foo.bar?.baz.z) && void 0 !== o && o;
",
            "
c.foo.bar?.baz.z ?? false;
",
        );
    }

    #[test]
    fn restores_nullish_coalescing_in_common_containers() {
        define_ast_inline_test(transform_ast)(
            "
var _foo_bar, _foo_bar1, _opts_foo, _this;
var { qux = (_foo_bar = foo.bar) !== null && _foo_bar !== void 0 ? _foo_bar : \"qux\" } = {};
function foo(foo, qux = (_foo_bar1 = foo.bar) !== null && _foo_bar1 !== void 0 ? _foo_bar1 : \"qux\") {}
function bar(bar, qux = bar !== null && bar !== void 0 ? bar : \"qux\") {}
function foo2(opts) {
  var value = (_opts_foo = opts.foo) !== null && _opts_foo !== void 0 ? _opts_foo : \"default\";
}
function foo3(foo, bar = foo !== null && foo !== void 0 ? foo : \"bar\") {}
function foo4() {
  var value = (_this = this) !== null && _this !== void 0 ? _this : {};
}
",
            "
var { qux = foo.bar ?? \"qux\" } = {};
function foo(foo, qux = foo.bar ?? \"qux\") {}
function bar(bar, qux = bar ?? \"qux\") {}
function foo2(opts) {
  var value = opts.foo ?? \"default\";
}
function foo3(foo, bar = foo ?? \"bar\") {}
function foo4() {
  var value = this ?? {};
}
",
        );
    }

    #[test]
    fn leaves_mismatched_guards_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
foo !== null && bar !== void 0 ? foo : \"bar\";
",
            "
foo !== null && bar !== void 0 ? foo : \"bar\";
",
        );
    }
}
