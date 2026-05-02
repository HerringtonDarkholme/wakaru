use std::collections::HashSet;

use oxc_allocator::{Box, CloneIn, TakeIn};
use oxc_ast::{
    ast::{
        AssignmentExpression, AssignmentTarget, BindingPattern, ConditionalExpression, Expression,
        FormalParameter, FormalParameters, Function, FunctionBody, IdentifierReference, Statement,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk, walk_mut, Visit, VisitMut};
use oxc_span::Span;
use oxc_syntax::{
    operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator},
    scope::ScopeFlags,
};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

const BODY_LENGTH_THRESHOLD: usize = 15;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut transformer = DefaultParameterTransformer {
        ast: AstBuilder::new(source.allocator),
    };

    transformer.visit_program(&mut source.program);

    Ok(())
}

struct DefaultParameterTransformer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for DefaultParameterTransformer<'a> {
    fn visit_function(&mut self, function: &mut Function<'a>, flags: ScopeFlags) {
        walk_mut::walk_function(self, function, flags);

        if let Some(body) = function.body.as_mut() {
            self.handle_body(&mut function.params, body);
        }
    }

    fn visit_arrow_function_expression(
        &mut self,
        arrow: &mut oxc_ast::ast::ArrowFunctionExpression<'a>,
    ) {
        walk_mut::walk_arrow_function_expression(self, arrow);
        self.handle_body(&mut arrow.params, &mut arrow.body);
    }
}

impl<'a> DefaultParameterTransformer<'a> {
    fn handle_body(&self, params: &mut FormalParameters<'a>, body: &mut FunctionBody<'a>) {
        if body.statements.is_empty() {
            return;
        }

        let mut used_names = collect_used_names(params, body);
        let old_statements = body.statements.take_in(self.ast);
        let mut new_statements = self.ast.vec_with_capacity(old_statements.len());
        let mut seen_identifier_references = HashSet::new();

        for (index, statement) in old_statements.into_iter().enumerate() {
            let previous_references = seen_identifier_references.clone();
            collect_statement_identifier_references(&statement, &mut seen_identifier_references);

            let should_remove = index < BODY_LENGTH_THRESHOLD
                && (self.apply_loose_default_statement(params, &statement, &previous_references)
                    || self.apply_arguments_parameter_statement(
                        params,
                        &statement,
                        &mut used_names,
                    ));

            if !should_remove {
                new_statements.push(statement);
            }
        }

        body.statements = new_statements;
    }

    fn apply_loose_default_statement(
        &self,
        params: &mut FormalParameters<'a>,
        statement: &Statement<'a>,
        previous_references: &HashSet<String>,
    ) -> bool {
        let Some((name, default_value)) = loose_default_statement(statement) else {
            return false;
        };

        if previous_references.contains(name) || existing_default_param(params, name) {
            return false;
        }

        let init = default_value.clone_in(self.ast.allocator);
        if let Some(parameter) = existing_param_mut(params, name) {
            parameter.initializer = Some(Box::new_in(init, self.ast.allocator));
        } else {
            params.items.push(self.formal_parameter(name, Some(init)));
        }

        true
    }

    fn apply_arguments_parameter_statement(
        &self,
        params: &mut FormalParameters<'a>,
        statement: &Statement<'a>,
        used_names: &mut HashSet<String>,
    ) -> bool {
        let Some((name, init)) = arguments_parameter_declaration(statement) else {
            return false;
        };

        if existing_default_param(params, name) {
            return false;
        }

        match init {
            ArgumentsParameterInit::Normal { index } => {
                if existing_param(params, name) {
                    return true;
                }

                self.insert_parameter_at(params, index, name, None, used_names);
                true
            }
            ArgumentsParameterInit::Default {
                index,
                default_value,
            } => {
                let init = default_value.clone_in(self.ast.allocator);
                if let Some(parameter) = existing_param_mut(params, name) {
                    parameter.initializer = Some(Box::new_in(init, self.ast.allocator));
                } else {
                    self.insert_parameter_at(params, index, name, Some(init), used_names);
                }

                true
            }
        }
    }

    fn insert_parameter_at(
        &self,
        params: &mut FormalParameters<'a>,
        index: usize,
        name: &str,
        initializer: Option<Expression<'a>>,
        used_names: &mut HashSet<String>,
    ) {
        while params.items.len() < index {
            let placeholder_index = params.items.len();
            let placeholder = unique_placeholder_name(placeholder_index, used_names);
            used_names.insert(placeholder.clone());
            params
                .items
                .push(self.formal_parameter(placeholder.as_str(), None));
        }

        used_names.insert(name.to_string());
        let parameter = self.formal_parameter(name, initializer);
        if index >= params.items.len() {
            params.items.push(parameter);
        } else {
            params.items.insert(index, parameter);
        }
    }

    fn formal_parameter(
        &self,
        name: &str,
        initializer: Option<Expression<'a>>,
    ) -> FormalParameter<'a> {
        self.ast.formal_parameter(
            Span::default(),
            self.ast.vec(),
            self.ast
                .binding_pattern_binding_identifier(Span::default(), self.ast.ident(name)),
            None::<Box<'a, oxc_ast::ast::TSTypeAnnotation<'a>>>,
            initializer,
            false,
            None,
            false,
            false,
        )
    }
}

enum ArgumentsParameterInit<'a, 'b> {
    Normal {
        index: usize,
    },
    Default {
        index: usize,
        default_value: &'b Expression<'a>,
    },
}

fn loose_default_statement<'a>(
    statement: &'a Statement<'a>,
) -> Option<(&'a str, &'a Expression<'a>)> {
    let Statement::IfStatement(if_statement) = statement else {
        return None;
    };
    if if_statement.alternate.is_some() {
        return None;
    }

    let name = strict_undefined_check(&if_statement.test)?;
    let assignment = assignment_expression_statement(&if_statement.consequent)?;
    if assignment.operator != AssignmentOperator::Assign {
        return None;
    }

    let AssignmentTarget::AssignmentTargetIdentifier(left) = &assignment.left else {
        return None;
    };
    if left.name != name {
        return None;
    }

    Some((name, &assignment.right))
}

fn assignment_expression_statement<'a>(
    statement: &'a Statement<'a>,
) -> Option<&'a AssignmentExpression<'a>> {
    match statement {
        Statement::ExpressionStatement(statement) => {
            let Expression::AssignmentExpression(assignment) = &statement.expression else {
                return None;
            };
            Some(assignment)
        }
        Statement::BlockStatement(block) if block.body.len() == 1 => {
            assignment_expression_statement(block.body.first()?)
        }
        _ => None,
    }
}

fn strict_undefined_check<'a>(expression: &'a Expression<'a>) -> Option<&'a str> {
    let Expression::BinaryExpression(binary) = without_parentheses(expression) else {
        return None;
    };
    if binary.operator != BinaryOperator::StrictEquality {
        return None;
    }

    match (
        identifier_name(&binary.left),
        is_undefined_expression(&binary.right),
    ) {
        (Some(name), true) => Some(name),
        _ => None,
    }
}

fn arguments_parameter_declaration<'a>(
    statement: &'a Statement<'a>,
) -> Option<(&'a str, ArgumentsParameterInit<'a, 'a>)> {
    let Statement::VariableDeclaration(declaration) = statement else {
        return None;
    };
    if declaration.declarations.len() != 1 {
        return None;
    }

    let declarator = declaration.declarations.first()?;
    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return None;
    };
    let Expression::ConditionalExpression(init) = declarator.init.as_ref()? else {
        return None;
    };

    if let Some(index) = normal_parameter_index(init) {
        return Some((
            identifier.name.as_str(),
            ArgumentsParameterInit::Normal { index },
        ));
    }

    let (index, default_value) = default_parameter_match(init)?;
    Some((
        identifier.name.as_str(),
        ArgumentsParameterInit::Default {
            index,
            default_value,
        },
    ))
}

fn normal_parameter_index(conditional: &ConditionalExpression) -> Option<usize> {
    let index = arguments_length_greater_than_index(&conditional.test)?;
    if arguments_member_index(&conditional.consequent)? != index {
        return None;
    }
    if !is_undefined_expression(&conditional.alternate) {
        return None;
    }

    Some(index)
}

fn default_parameter_match<'a, 'b>(
    conditional: &'b ConditionalExpression<'a>,
) -> Option<(usize, &'b Expression<'a>)> {
    let Expression::LogicalExpression(logical) = without_parentheses(&conditional.test) else {
        return None;
    };
    if logical.operator != LogicalOperator::And {
        return None;
    }

    let index = arguments_length_greater_than_index(&logical.left)?;
    if arguments_member_not_undefined_index(&logical.right)? != index {
        return None;
    }
    if arguments_member_index(&conditional.consequent)? != index {
        return None;
    }
    if is_undefined_expression(&conditional.alternate) {
        return None;
    }

    Some((index, &conditional.alternate))
}

fn arguments_length_greater_than_index(expression: &Expression) -> Option<usize> {
    let Expression::BinaryExpression(binary) = without_parentheses(expression) else {
        return None;
    };
    if binary.operator != BinaryOperator::GreaterThan || !is_arguments_length(&binary.left) {
        return None;
    }

    numeric_index(&binary.right)
}

fn arguments_member_not_undefined_index(expression: &Expression) -> Option<usize> {
    let Expression::BinaryExpression(binary) = without_parentheses(expression) else {
        return None;
    };
    if binary.operator != BinaryOperator::StrictInequality {
        return None;
    }

    if is_undefined_expression(&binary.right) {
        return arguments_member_index(&binary.left);
    }
    if is_undefined_expression(&binary.left) {
        return arguments_member_index(&binary.right);
    }

    None
}

fn is_arguments_length(expression: &Expression) -> bool {
    let Expression::StaticMemberExpression(member) = without_parentheses(expression) else {
        return false;
    };

    identifier_name(&member.object) == Some("arguments") && member.property.name == "length"
}

fn arguments_member_index(expression: &Expression) -> Option<usize> {
    let Expression::ComputedMemberExpression(member) = without_parentheses(expression) else {
        return None;
    };
    if identifier_name(&member.object) != Some("arguments") {
        return None;
    }

    numeric_index(&member.expression)
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

fn is_undefined_expression(expression: &Expression) -> bool {
    match without_parentheses(expression) {
        Expression::Identifier(identifier) => identifier.name == "undefined",
        Expression::UnaryExpression(unary) => {
            unary.operator == UnaryOperator::Void
                && matches!(&unary.argument, Expression::NumericLiteral(number) if number.value == 0.0)
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

fn without_parentheses<'a, 'b>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            without_parentheses(&parenthesized.expression)
        }
        _ => expression,
    }
}

fn existing_param(params: &FormalParameters, name: &str) -> bool {
    params
        .items
        .iter()
        .any(|parameter| parameter_identifier_name(parameter) == Some(name))
}

fn existing_param_mut<'a, 'b>(
    params: &'b mut FormalParameters<'a>,
    name: &str,
) -> Option<&'b mut FormalParameter<'a>> {
    params
        .items
        .iter_mut()
        .find(|parameter| parameter_identifier_name(parameter) == Some(name))
}

fn existing_default_param(params: &FormalParameters, name: &str) -> bool {
    params.items.iter().any(|parameter| {
        parameter.initializer.is_some() && parameter_identifier_name(parameter) == Some(name)
    })
}

fn parameter_identifier_name<'a>(parameter: &'a FormalParameter) -> Option<&'a str> {
    let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
        return None;
    };

    Some(identifier.name.as_str())
}

fn collect_used_names(params: &FormalParameters, body: &FunctionBody) -> HashSet<String> {
    let mut names = HashSet::new();
    for parameter in &params.items {
        if let Some(name) = parameter_identifier_name(parameter) {
            names.insert(name.to_string());
        }
    }

    let mut collector = BindingNameCollector { names: &mut names };
    collector.visit_function_body(body);

    names
}

fn collect_statement_identifier_references(statement: &Statement, names: &mut HashSet<String>) {
    let mut collector = IdentifierReferenceCollector { names };
    collector.visit_statement(statement);
}

fn unique_placeholder_name(index: usize, used_names: &HashSet<String>) -> String {
    let base = format!("_param_{index}");
    if !used_names.contains(&base) {
        return base;
    }

    let mut suffix = 1;
    loop {
        let candidate = format!("{base}_{suffix}");
        if !used_names.contains(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

struct BindingNameCollector<'b> {
    names: &'b mut HashSet<String>,
}

impl<'a> Visit<'a> for BindingNameCollector<'_> {
    fn visit_binding_identifier(&mut self, identifier: &oxc_ast::ast::BindingIdentifier<'a>) {
        self.names.insert(identifier.name.as_str().to_string());
        walk::walk_binding_identifier(self, identifier);
    }
}

struct IdentifierReferenceCollector<'b> {
    names: &'b mut HashSet<String>,
}

impl<'a> Visit<'a> for IdentifierReferenceCollector<'_> {
    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        self.names.insert(identifier.name.as_str().to_string());
        walk::walk_identifier_reference(self, identifier);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn restores_loose_default_parameters() {
        define_ast_inline_test(transform_ast)(
            "
function test(x, y) {
  if (x === void 0) x = 1;
  if (y === undefined) {
    y = 2;
  }
  console.log(x, y);
}
",
            "
function test(x = 1, y = 2) {
  console.log(x, y);
}
",
        );
    }

    #[test]
    fn restores_arguments_parameters() {
        define_ast_inline_test(transform_ast)(
            r#"
function add2() {
  var a = arguments.length > 0 && arguments[0] !== undefined ? arguments[0] : 2;
  var b = arguments.length > 1 ? arguments[1] : undefined;
  return a + b;
}
"#,
            r#"
function add2(a = 2, b) {
  return a + b;
}
"#,
        );
    }

    #[test]
    fn fills_parameter_gaps_with_unique_placeholders() {
        define_ast_inline_test(transform_ast)(
            "
function test(a) {
  var e = arguments.length > 4 && arguments[4] !== undefined ? arguments[4] : world();
  var _param_2 = 1;
  return e;
}
",
            "
function test(a, _param_1, _param_2_1, _param_3, e = world()) {
  var _param_2 = 1;
  return e;
}
",
        );
    }

    #[test]
    fn leaves_parameter_when_used_before_default_statement() {
        define_ast_inline_test(transform_ast)(
            "
function test(a, b) {
  if (a === void 0) a = 1;
  console.log(b);
  if (b === void 0) b = 2;
}
",
            "
function test(a = 1, b) {
  console.log(b);
  if (b === void 0) b = 2;
}
",
        );
    }
}
