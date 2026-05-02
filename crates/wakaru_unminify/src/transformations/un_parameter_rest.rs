use oxc_allocator::{Box, TakeIn};
use oxc_ast::{
    ast::{
        Argument, AssignmentExpression, AssignmentTarget, BindingPattern, Expression, ForStatement,
        ForStatementInit, Function, IdentifierReference, SimpleAssignmentTarget, Statement,
        VariableDeclaration, VariableDeclarationKind, VariableDeclarator,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk, walk_mut, Visit, VisitMut};
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_span::Span;
use oxc_syntax::{
    operator::{AssignmentOperator, BinaryOperator, UpdateOperator},
    scope::ScopeFlags,
};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();

    let mut transformer = ParameterRestTransformer {
        ast: AstBuilder::new(source.allocator),
        scoping,
    };

    transformer.visit_program(&mut source.program);

    Ok(())
}

struct ParameterRestTransformer<'a> {
    ast: AstBuilder<'a>,
    scoping: Scoping,
}

impl<'a> VisitMut<'a> for ParameterRestTransformer<'a> {
    fn visit_function(&mut self, function: &mut Function<'a>, flags: ScopeFlags) {
        walk_mut::walk_function(self, function, flags);
        self.try_transform_function(function);
    }
}

impl<'a> ParameterRestTransformer<'a> {
    fn try_transform_function(&self, function: &mut Function<'a>) {
        if function.params.rest.is_some() {
            return;
        }

        if self.try_transform_generated_rest_loop(function) {
            return;
        }

        if !function.params.items.is_empty() {
            return;
        }

        let Some(body) = function.body.as_mut() else {
            return;
        };
        let Some(scope_id) = function.scope_id.get() else {
            return;
        };

        if self.scoping.find_binding(scope_id, "args".into()).is_some()
            || self
                .scoping
                .find_binding(scope_id, "arguments".into())
                .is_some()
        {
            return;
        }

        let mut scanner = ArgumentsReferenceScanner {
            scoping: &self.scoping,
            found: false,
            has_args_conflict: false,
        };
        scanner.visit_function_body(body);

        if !scanner.found || scanner.has_args_conflict {
            return;
        }

        let mut renamer = ArgumentsRenamer { ast: self.ast };
        renamer.visit_function_body(body);

        function.params.rest = Some(self.rest_parameter("args"));
    }

    fn try_transform_generated_rest_loop(&self, function: &mut Function<'a>) -> bool {
        let Some(body) = function.body.as_mut() else {
            return false;
        };
        let Some((statement_index, rest_loop)) =
            body.statements
                .iter()
                .enumerate()
                .find_map(|(index, statement)| {
                    generated_rest_loop(statement).map(|rest| (index, rest))
                })
        else {
            return false;
        };

        if function.params.items.len() != rest_loop.start_index
            || function.params.items.iter().any(|parameter| {
                parameter_identifier_name(parameter) == Some(rest_loop.name.as_str())
            })
        {
            return false;
        }

        let old_statements = body.statements.take_in(self.ast);
        let mut new_statements = self
            .ast
            .vec_with_capacity(old_statements.len().saturating_sub(1));
        for (index, statement) in old_statements.into_iter().enumerate() {
            if index != statement_index {
                new_statements.push(statement);
            }
        }
        body.statements = new_statements;

        function.params.rest = Some(self.rest_parameter(rest_loop.name.as_str()));

        true
    }

    fn rest_parameter(&self, name: &str) -> Box<'a, oxc_ast::ast::FormalParameterRest<'a>> {
        self.ast.alloc_formal_parameter_rest(
            Span::default(),
            self.ast.vec(),
            self.ast.binding_rest_element(
                Span::default(),
                self.ast
                    .binding_pattern_binding_identifier(Span::default(), self.ast.ident(name)),
            ),
            None::<Box<'a, oxc_ast::ast::TSTypeAnnotation<'a>>>,
        )
    }
}

struct GeneratedRestLoop {
    name: String,
    start_index: usize,
}

fn generated_rest_loop(statement: &Statement) -> Option<GeneratedRestLoop> {
    let Statement::ForStatement(for_statement) = statement else {
        return None;
    };
    let declaration = for_var_declaration(for_statement)?;
    if declaration.declarations.len() != 3 {
        return None;
    }

    let len_declarator = &declaration.declarations[0];
    let rest_declarator = &declaration.declarations[1];
    let key_declarator = &declaration.declarations[2];

    let len_name = binding_identifier_name(len_declarator)?;
    if !len_declarator
        .init
        .as_ref()
        .is_some_and(is_arguments_length)
    {
        return None;
    }

    let rest_name = binding_identifier_name(rest_declarator)?;
    let key_name = binding_identifier_name(key_declarator)?;
    let start_index = numeric_index(key_declarator.init.as_ref()?)?;

    if !is_rest_array_init(rest_declarator.init.as_ref()?, len_name, start_index)
        || !is_key_less_than_len(for_statement.test.as_ref()?, key_name, len_name)
        || !is_key_increment(for_statement.update.as_ref()?, key_name)
        || !is_rest_copy_body(&for_statement.body, rest_name, key_name, start_index)
    {
        return None;
    }

    Some(GeneratedRestLoop {
        name: rest_name.to_string(),
        start_index,
    })
}

fn for_var_declaration<'a, 'b>(
    for_statement: &'b ForStatement<'a>,
) -> Option<&'b VariableDeclaration<'a>> {
    let Some(ForStatementInit::VariableDeclaration(declaration)) = &for_statement.init else {
        return None;
    };
    if declaration.kind != VariableDeclarationKind::Var {
        return None;
    }

    Some(declaration)
}

fn is_rest_array_init(expression: &Expression, len_name: &str, start_index: usize) -> bool {
    let Expression::NewExpression(new_expression) = without_parentheses(expression) else {
        return false;
    };
    if !is_identifier(&new_expression.callee, "Array") || new_expression.arguments.len() != 1 {
        return false;
    }

    match &new_expression.arguments[0] {
        Argument::Identifier(identifier) => {
            start_index == 0 && identifier.name.as_str() == len_name
        }
        Argument::ConditionalExpression(conditional) => {
            if start_index == 0 {
                return false;
            }
            is_len_greater_than_start(&conditional.test, len_name, start_index)
                && is_len_minus_start(&conditional.consequent, len_name, start_index)
                && numeric_index(&conditional.alternate) == Some(0)
        }
        _ => false,
    }
}

fn is_key_less_than_len(expression: &Expression, key_name: &str, len_name: &str) -> bool {
    let Expression::BinaryExpression(binary) = without_parentheses(expression) else {
        return false;
    };

    binary.operator == BinaryOperator::LessThan
        && is_identifier(&binary.left, key_name)
        && is_identifier(&binary.right, len_name)
}

fn is_key_increment(expression: &Expression, key_name: &str) -> bool {
    let Expression::UpdateExpression(update) = without_parentheses(expression) else {
        return false;
    };
    if update.operator != UpdateOperator::Increment {
        return false;
    }

    matches!(
        &update.argument,
        SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier)
            if identifier.name.as_str() == key_name
    )
}

fn is_rest_copy_body(
    statement: &Statement,
    rest_name: &str,
    key_name: &str,
    start_index: usize,
) -> bool {
    let Statement::BlockStatement(block) = statement else {
        return false;
    };
    if block.body.len() != 1 {
        return false;
    }

    let Some(assignment) = assignment_expression_statement(&block.body[0]) else {
        return false;
    };
    if assignment.operator != AssignmentOperator::Assign {
        return false;
    }

    is_rest_assignment_target(&assignment.left, rest_name, key_name, start_index)
        && is_arguments_key_member(&assignment.right, key_name)
}

fn assignment_expression_statement<'a, 'b>(
    statement: &'b Statement<'a>,
) -> Option<&'b AssignmentExpression<'a>> {
    let Statement::ExpressionStatement(statement) = statement else {
        return None;
    };
    let Expression::AssignmentExpression(assignment) = &statement.expression else {
        return None;
    };

    Some(assignment)
}

fn is_rest_assignment_target(
    target: &AssignmentTarget,
    rest_name: &str,
    key_name: &str,
    start_index: usize,
) -> bool {
    let AssignmentTarget::ComputedMemberExpression(member) = target else {
        return false;
    };

    is_identifier(&member.object, rest_name)
        && is_rest_target_index(&member.expression, key_name, start_index)
}

fn is_rest_target_index(expression: &Expression, key_name: &str, start_index: usize) -> bool {
    if start_index == 0 {
        return is_identifier(expression, key_name);
    }

    is_key_minus_start(expression, key_name, start_index)
}

fn is_arguments_key_member(expression: &Expression, key_name: &str) -> bool {
    let Expression::ComputedMemberExpression(member) = without_parentheses(expression) else {
        return false;
    };

    is_identifier(&member.object, "arguments") && is_identifier(&member.expression, key_name)
}

fn is_len_greater_than_start(expression: &Expression, len_name: &str, start_index: usize) -> bool {
    let Expression::BinaryExpression(binary) = without_parentheses(expression) else {
        return false;
    };

    binary.operator == BinaryOperator::GreaterThan
        && is_identifier(&binary.left, len_name)
        && numeric_index(&binary.right) == Some(start_index)
}

fn is_len_minus_start(expression: &Expression, len_name: &str, start_index: usize) -> bool {
    is_identifier_minus_number(expression, len_name, start_index)
}

fn is_key_minus_start(expression: &Expression, key_name: &str, start_index: usize) -> bool {
    is_identifier_minus_number(expression, key_name, start_index)
}

fn is_identifier_minus_number(expression: &Expression, name: &str, number: usize) -> bool {
    let Expression::BinaryExpression(binary) = without_parentheses(expression) else {
        return false;
    };

    binary.operator == BinaryOperator::Subtraction
        && is_identifier(&binary.left, name)
        && numeric_index(&binary.right) == Some(number)
}

fn is_arguments_length(expression: &Expression) -> bool {
    let Expression::StaticMemberExpression(member) = without_parentheses(expression) else {
        return false;
    };

    is_identifier(&member.object, "arguments") && member.property.name.as_str() == "length"
}

fn numeric_index(expression: &Expression) -> Option<usize> {
    let Expression::NumericLiteral(number) = without_parentheses(expression) else {
        return None;
    };
    if number.value < 0.0 || number.value.fract() != 0.0 {
        return None;
    }

    Some(number.value as usize)
}

fn is_identifier(expression: &Expression, name: &str) -> bool {
    matches!(without_parentheses(expression), Expression::Identifier(identifier) if identifier.name.as_str() == name)
}

fn binding_identifier_name<'a>(declarator: &'a VariableDeclarator<'a>) -> Option<&'a str> {
    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return None;
    };

    Some(identifier.name.as_str())
}

fn parameter_identifier_name<'a>(parameter: &'a oxc_ast::ast::FormalParameter) -> Option<&'a str> {
    let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
        return None;
    };

    Some(identifier.name.as_str())
}

fn without_parentheses<'a, 'b>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            without_parentheses(&parenthesized.expression)
        }
        _ => expression,
    }
}

struct ArgumentsReferenceScanner<'s> {
    scoping: &'s Scoping,
    found: bool,
    has_args_conflict: bool,
}

impl<'a> Visit<'a> for ArgumentsReferenceScanner<'_> {
    fn visit_function(&mut self, _function: &Function<'a>, _flags: ScopeFlags) {}

    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        if identifier.name != "arguments" {
            walk::walk_identifier_reference(self, identifier);
            return;
        }

        self.found = true;

        let Some(reference_id) = identifier.reference_id.get() else {
            self.has_args_conflict = true;
            return;
        };
        let reference_scope_id = self.scoping.get_reference(reference_id).scope_id();
        if self
            .scoping
            .find_binding(reference_scope_id, "args".into())
            .is_some()
        {
            self.has_args_conflict = true;
        }
    }
}

struct ArgumentsRenamer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for ArgumentsRenamer<'a> {
    fn visit_function(&mut self, _function: &mut Function<'a>, _flags: ScopeFlags) {}

    fn visit_identifier_reference(&mut self, identifier: &mut IdentifierReference<'a>) {
        if identifier.name == "arguments" {
            identifier.name = self.ast.ident("args");
            return;
        }

        walk_mut::walk_identifier_reference(self, identifier);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn replaces_arguments_in_function_declaration() {
        define_ast_inline_test(transform_ast)(
            "
function foo() {
  console.log(arguments);
}
",
            "
function foo(...args) {
  console.log(args);
}
",
        );
    }

    #[test]
    fn replaces_arguments_in_function_expression_and_nested_arrow() {
        define_ast_inline_test(transform_ast)(
            "
var foo = function() {
  var bar = () => console.log(arguments);
}
",
            "
var foo = function(...args) {
  var bar = () => console.log(args);
};
",
        );
    }

    #[test]
    fn restores_generated_rest_loop_without_formal_params() {
        define_ast_inline_test(transform_ast)(
            "
function foo() {
  for (var _len = arguments.length, args = new Array(_len), _key = 0; _key < _len; _key++) {
    args[_key] = arguments[_key];
  }
  args.pop();
  foo.apply(void 0, args);
}
",
            "
function foo(...args) {
  args.pop();
  foo.apply(void 0, args);
}
",
        );
    }

    #[test]
    fn restores_generated_rest_loop_after_formal_param() {
        define_ast_inline_test(transform_ast)(
            "
function foo(first) {
  for (var _len = arguments.length, args = new Array(_len > 1 ? _len - 1 : 0), _key = 1; _key < _len; _key++) {
    args[_key - 1] = arguments[_key];
  }
  return args.length + first;
}
",
            "
function foo(first, ...args) {
  return args.length + first;
}
",
        );
    }

    #[test]
    fn leaves_arrow_function_and_existing_params_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
var foo = () => console.log(arguments);
function bar(a) {
  console.log(arguments);
}
",
            "
var foo = () => console.log(arguments);
function bar(a) {
  console.log(arguments);
}
",
        );
    }

    #[test]
    fn skips_args_conflicts() {
        define_ast_inline_test(transform_ast)(
            "
var args = [];
function foo() {
  console.log(args, arguments);
}
function bar() {
  if (true) {
    const args = 0;
    console.log(arguments);
  }
}
",
            "
var args = [];
function foo() {
  console.log(args, arguments);
}
function bar() {
  if (true) {
    const args = 0;
    console.log(arguments);
  }
}
",
        );
    }
}
