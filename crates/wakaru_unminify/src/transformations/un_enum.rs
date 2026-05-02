use std::collections::{HashMap, VecDeque};

use oxc_allocator::{CloneIn, TakeIn};
use oxc_ast::{
    ast::{
        Argument, AssignmentTarget, BindingPattern, CallExpression, Expression, ObjectPropertyKind,
        Program, PropertyKey, PropertyKind, Statement, VariableDeclaration,
    },
    AstBuilder,
};
use oxc_span::Span;
use oxc_syntax::operator::{AssignmentOperator, LogicalOperator, UnaryOperator};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::{ParsedSourceFile, SyntheticTrailingComment};

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let ast = AstBuilder::new(source.allocator);
    let mut transformer = EnumTransformer {
        ast,
        enum_counts: HashMap::new(),
        synthetic_replacements: &mut source.synthetic_trailing_comments,
    };

    transformer.transform_program(&mut source.program);

    Ok(())
}

struct EnumTransformer<'a, 'b> {
    ast: AstBuilder<'a>,
    enum_counts: HashMap<String, usize>,
    synthetic_replacements: &'b mut Vec<SyntheticTrailingComment>,
}

impl<'a> EnumTransformer<'a, '_> {
    fn transform_program(&mut self, program: &mut Program<'a>) {
        let old_body = program.body.take_in(self.ast);
        let mut new_body = self.ast.vec_with_capacity(old_body.len());
        let mut old_body: VecDeque<_> = old_body.into_iter().collect();

        while let Some(statement) = old_body.pop_front() {
            let statement =
                match self.transform_compressed_swc_enum_with_front(statement, &mut old_body) {
                    Ok(transformed) => {
                        new_body.push(transformed);
                        continue;
                    }
                    Err(statement) => statement,
                };

            let statement =
                match self.transform_variable_declaration_with_next(statement, old_body.front()) {
                    Ok(transformed) => {
                        new_body.push(transformed);
                        old_body.pop_front();
                        continue;
                    }
                    Err(statement) => statement,
                };

            let statement = match self.transform_statement(statement) {
                Ok(statement) => statement,
                Err(statement) => statement,
            };
            new_body.push(statement);
        }

        program.body = new_body;
    }

    fn transform_variable_declaration_with_next(
        &mut self,
        statement: Statement<'a>,
        next: Option<&Statement<'a>>,
    ) -> std::result::Result<Statement<'a>, Statement<'a>> {
        let Statement::VariableDeclaration(mut declaration) = statement else {
            return Err(statement);
        };

        let Some(enum_name) = single_uninitialized_binding_name(&declaration) else {
            return Err(Statement::VariableDeclaration(declaration));
        };
        let Some(next) = next else {
            return Err(Statement::VariableDeclaration(declaration));
        };
        if iife_external_name(next) != Some(enum_name.as_str()) {
            return Err(Statement::VariableDeclaration(declaration));
        }

        let Some(object) = enum_object_from_statement_iife(
            next,
            self.ast,
            &enum_name,
            false,
            self.synthetic_replacements,
        ) else {
            return Err(Statement::VariableDeclaration(declaration));
        };
        declaration.declarations[0].init = Some(object);
        self.mark_enum_seen(&enum_name);
        Ok(Statement::VariableDeclaration(declaration))
    }

    fn transform_compressed_swc_enum_with_front(
        &mut self,
        statement: Statement<'a>,
        rest: &mut VecDeque<Statement<'a>>,
    ) -> std::result::Result<Statement<'a>, Statement<'a>> {
        let Statement::VariableDeclaration(mut declaration) = statement else {
            return Err(statement);
        };

        let Some(enum_name) = single_uninitialized_binding_name(&declaration) else {
            return Err(Statement::VariableDeclaration(declaration));
        };
        let Some(alias_statement) = rest.front() else {
            return Err(Statement::VariableDeclaration(declaration));
        };
        let Some(alias_name) = single_uninitialized_binding_name_from_statement(alias_statement)
        else {
            return Err(Statement::VariableDeclaration(declaration));
        };
        let Some(setup_statement) = rest.get(1) else {
            return Err(Statement::VariableDeclaration(declaration));
        };
        if !is_compressed_swc_alias_assignment(setup_statement, &alias_name, &enum_name) {
            return Err(Statement::VariableDeclaration(declaration));
        }

        let should_add_spread = self.enum_counts.contains_key(&enum_name);
        let Some((object, consumed_assignments)) = enum_object_from_compressed_assignments(
            rest,
            self.ast,
            &alias_name,
            &enum_name,
            should_add_spread,
            self.synthetic_replacements,
        ) else {
            return Err(Statement::VariableDeclaration(declaration));
        };

        rest.pop_front();
        rest.pop_front();
        for _ in 0..consumed_assignments {
            rest.pop_front();
        }

        declaration.declarations[0].init = Some(object);
        self.mark_enum_seen(&enum_name);
        Ok(Statement::VariableDeclaration(declaration))
    }

    fn transform_statement(
        &mut self,
        statement: Statement<'a>,
    ) -> std::result::Result<Statement<'a>, Statement<'a>> {
        match statement {
            Statement::VariableDeclaration(declaration) => {
                self.transform_initialized_variable_declaration(declaration)
            }
            Statement::ExpressionStatement(statement) => {
                let Some(enum_name) =
                    iife_external_name_from_expression(&statement.expression).map(str::to_string)
                else {
                    return Err(Statement::ExpressionStatement(statement));
                };

                let should_add_spread = self.enum_counts.contains_key(&enum_name);
                let Some(object) = enum_object_from_expression_iife(
                    &statement.expression,
                    self.ast,
                    &enum_name,
                    should_add_spread,
                    self.synthetic_replacements,
                ) else {
                    return Err(Statement::ExpressionStatement(statement));
                };

                self.mark_enum_seen(&enum_name);
                Ok(assignment_statement(self.ast, &enum_name, object))
            }
            statement => Err(statement),
        }
    }

    fn transform_initialized_variable_declaration(
        &mut self,
        mut declaration: oxc_allocator::Box<'a, VariableDeclaration<'a>>,
    ) -> std::result::Result<Statement<'a>, Statement<'a>> {
        if declaration.declarations.len() != 1 {
            return Err(Statement::VariableDeclaration(declaration));
        }

        let declarator = &declaration.declarations[0];
        let BindingPattern::BindingIdentifier(id) = &declarator.id else {
            return Err(Statement::VariableDeclaration(declaration));
        };
        let enum_name = id.name.as_str().to_string();
        let Some(init) = &declarator.init else {
            return Err(Statement::VariableDeclaration(declaration));
        };
        if iife_external_name_from_expression(init) != Some(enum_name.as_str()) {
            return Err(Statement::VariableDeclaration(declaration));
        }

        let should_add_spread = self.enum_counts.contains_key(&enum_name);
        let Some(object) = enum_object_from_expression_iife(
            init,
            self.ast,
            &enum_name,
            should_add_spread,
            self.synthetic_replacements,
        ) else {
            return Err(Statement::VariableDeclaration(declaration));
        };

        declaration.declarations[0].init = Some(object);
        self.mark_enum_seen(&enum_name);
        Ok(Statement::VariableDeclaration(declaration))
    }

    fn mark_enum_seen(&mut self, enum_name: &str) {
        *self.enum_counts.entry(enum_name.to_string()).or_default() += 1;
    }
}

fn single_uninitialized_binding_name(declaration: &VariableDeclaration) -> Option<String> {
    if declaration.declarations.len() != 1 {
        return None;
    }

    let declarator = &declaration.declarations[0];
    if declarator.init.is_some() {
        return None;
    }

    let BindingPattern::BindingIdentifier(id) = &declarator.id else {
        return None;
    };
    Some(id.name.as_str().to_string())
}

fn single_uninitialized_binding_name_from_statement(statement: &Statement) -> Option<String> {
    let Statement::VariableDeclaration(declaration) = statement else {
        return None;
    };
    single_uninitialized_binding_name(declaration)
}

fn iife_external_name<'b, 'a>(statement: &'b Statement<'a>) -> Option<&'b str> {
    let Statement::ExpressionStatement(statement) = statement else {
        return None;
    };
    iife_external_name_from_expression(&statement.expression)
}

fn is_compressed_swc_alias_assignment(
    statement: &Statement,
    alias_name: &str,
    enum_name: &str,
) -> bool {
    let Some(assignment) = expression_statement_assignment(statement) else {
        return false;
    };
    if assignment_target_identifier_name(&assignment.left) != Some(alias_name) {
        return false;
    }

    let Expression::LogicalExpression(logical) = without_parentheses(&assignment.right) else {
        return false;
    };
    if logical.operator != LogicalOperator::Or || identifier_name(&logical.left) != Some(enum_name)
    {
        return false;
    }

    let Expression::AssignmentExpression(fallback) = without_parentheses(&logical.right) else {
        return false;
    };
    if fallback.operator != AssignmentOperator::Assign {
        return false;
    }
    if assignment_target_identifier_name(&fallback.left) != Some(enum_name) {
        return false;
    }

    matches!(
        without_parentheses(&fallback.right),
        Expression::ObjectExpression(object) if object.properties.is_empty()
    )
}

fn iife_external_name_from_expression<'b, 'a>(expression: &'b Expression<'a>) -> Option<&'b str> {
    let call = iife_call_expression(expression)?;
    let argument = call.arguments.first()?;
    let Argument::LogicalExpression(logical) = argument else {
        return None;
    };
    if logical.operator != LogicalOperator::Or {
        return None;
    }

    let Expression::Identifier(left) = &logical.left else {
        return None;
    };
    let external_name = left.name.as_str();

    match without_parentheses(&logical.right) {
        Expression::AssignmentExpression(assignment)
            if assignment.operator == AssignmentOperator::Assign =>
        {
            let AssignmentTarget::AssignmentTargetIdentifier(right_id) = &assignment.left else {
                return None;
            };
            let Expression::ObjectExpression(object) = &assignment.right else {
                return None;
            };
            if !object.properties.is_empty() || right_id.name != external_name {
                return None;
            }
        }
        Expression::ObjectExpression(object) if object.properties.is_empty() => {}
        _ => return None,
    }

    iife_internal_name(call)?;
    Some(external_name)
}

fn enum_object_from_statement_iife<'a>(
    statement: &Statement<'a>,
    ast: AstBuilder<'a>,
    enum_name: &str,
    should_add_spread: bool,
    synthetic_replacements: &mut Vec<SyntheticTrailingComment>,
) -> Option<Expression<'a>> {
    let Statement::ExpressionStatement(statement) = statement else {
        return None;
    };
    enum_object_from_expression_iife(
        &statement.expression,
        ast,
        enum_name,
        should_add_spread,
        synthetic_replacements,
    )
}

fn enum_object_from_expression_iife<'a>(
    expression: &Expression<'a>,
    ast: AstBuilder<'a>,
    enum_name: &str,
    should_add_spread: bool,
    synthetic_replacements: &mut Vec<SyntheticTrailingComment>,
) -> Option<Expression<'a>> {
    let call = iife_call_expression(expression)?;
    if iife_external_name_from_expression(expression) != Some(enum_name) {
        return None;
    }

    let internal_name = iife_internal_name(call)?;
    let statements = iife_body_statements(call)?;
    let mut forward_properties = ast.vec();
    let mut reverse_properties = ast.vec();

    for statement in statements {
        if is_returning_internal_name(statement, internal_name) {
            continue;
        }

        let assignment = expression_statement_assignment(statement)?;
        let right_name = string_literal_value(&assignment.right)?;

        if let Some(property) =
            direct_string_enum_property(assignment, ast, internal_name, right_name)
        {
            forward_properties.push(property);
            continue;
        }

        let (forward_property, reverse_property) =
            numeric_enum_properties(assignment, ast, internal_name, right_name)?;
        forward_properties.push(forward_property);
        reverse_properties.push(reverse_property);
    }

    if forward_properties.is_empty() {
        return None;
    }

    let mut properties = ast.vec();
    if should_add_spread {
        properties.push(enum_spread_property(ast, enum_name, synthetic_replacements));
    }
    properties.extend(forward_properties);
    properties.extend(reverse_properties);

    Some(ast.expression_object(Span::default(), properties))
}

fn enum_object_from_compressed_assignments<'a>(
    statements: &VecDeque<Statement<'a>>,
    ast: AstBuilder<'a>,
    alias_name: &str,
    enum_name: &str,
    should_add_spread: bool,
    synthetic_replacements: &mut Vec<SyntheticTrailingComment>,
) -> Option<(Expression<'a>, usize)> {
    let mut forward_properties = ast.vec();
    let mut reverse_properties = ast.vec();
    let mut consumed_assignments = 0;

    for statement in statements.iter().skip(2) {
        let Some(assignment) = expression_statement_assignment(statement) else {
            break;
        };
        let Some(right_name) = string_literal_value(&assignment.right) else {
            break;
        };

        if let Some(property) = direct_string_enum_property(assignment, ast, alias_name, right_name)
        {
            forward_properties.push(property);
            consumed_assignments += 1;
            continue;
        }

        if let Some((forward_property, reverse_property)) =
            numeric_enum_properties(assignment, ast, alias_name, right_name)
        {
            forward_properties.push(forward_property);
            reverse_properties.push(reverse_property);
            consumed_assignments += 1;
            continue;
        }

        break;
    }

    if forward_properties.is_empty() {
        return None;
    }

    let mut properties = ast.vec();
    if should_add_spread {
        properties.push(enum_spread_property(ast, enum_name, synthetic_replacements));
    }
    properties.extend(forward_properties);
    properties.extend(reverse_properties);

    Some((
        ast.expression_object(Span::default(), properties),
        consumed_assignments,
    ))
}

fn iife_call_expression<'b, 'a>(expression: &'b Expression<'a>) -> Option<&'b CallExpression<'a>> {
    match without_parentheses(expression) {
        Expression::CallExpression(call) => Some(call),
        Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::LogicalNot => {
            match without_parentheses(&unary.argument) {
                Expression::CallExpression(call) => Some(call),
                _ => None,
            }
        }
        _ => None,
    }
}

fn iife_internal_name<'b, 'a>(call: &'b CallExpression<'a>) -> Option<&'b str> {
    match without_parentheses(&call.callee) {
        Expression::FunctionExpression(function) => function_parameter_name(&function.params),
        Expression::ArrowFunctionExpression(arrow) => function_parameter_name(&arrow.params),
        _ => None,
    }
}

fn function_parameter_name<'b, 'a>(
    params: &'b oxc_ast::ast::FormalParameters<'a>,
) -> Option<&'b str> {
    if params.items.len() != 1 || params.rest.is_some() {
        return None;
    }

    let BindingPattern::BindingIdentifier(id) = &params.items[0].pattern else {
        return None;
    };
    Some(id.name.as_str())
}

fn iife_body_statements<'b, 'a>(
    call: &'b CallExpression<'a>,
) -> Option<&'b oxc_allocator::Vec<'a, Statement<'a>>> {
    match without_parentheses(&call.callee) {
        Expression::FunctionExpression(function) => {
            let body = function.body.as_ref()?;
            Some(&body.statements)
        }
        Expression::ArrowFunctionExpression(arrow) if !arrow.expression => {
            Some(&arrow.body.statements)
        }
        _ => None,
    }
}

fn is_returning_internal_name(statement: &Statement, internal_name: &str) -> bool {
    let Statement::ReturnStatement(statement) = statement else {
        return false;
    };
    matches!(&statement.argument, Some(Expression::Identifier(id)) if id.name == internal_name)
}

fn expression_statement_assignment<'b, 'a>(
    statement: &'b Statement<'a>,
) -> Option<&'b oxc_ast::ast::AssignmentExpression<'a>> {
    let Statement::ExpressionStatement(statement) = statement else {
        return None;
    };
    let Expression::AssignmentExpression(assignment) = &statement.expression else {
        return None;
    };
    (assignment.operator == AssignmentOperator::Assign).then_some(assignment)
}

fn direct_string_enum_property<'a>(
    assignment: &oxc_ast::ast::AssignmentExpression<'a>,
    ast: AstBuilder<'a>,
    internal_name: &str,
    _right_name: &str,
) -> Option<ObjectPropertyKind<'a>> {
    let (object_name, key) = assignment_member_key(&assignment.left)?;
    if object_name != internal_name || key.is_assignment_expression() {
        return None;
    }

    let (key, computed) = key.into_property_key(ast)?;
    Some(object_property(
        ast,
        key,
        assignment.right.clone_in(ast.allocator),
        computed,
    ))
}

fn numeric_enum_properties<'a>(
    assignment: &oxc_ast::ast::AssignmentExpression<'a>,
    ast: AstBuilder<'a>,
    internal_name: &str,
    right_name: &str,
) -> Option<(ObjectPropertyKind<'a>, ObjectPropertyKind<'a>)> {
    let AssignmentTarget::ComputedMemberExpression(outer_member) = &assignment.left else {
        return None;
    };
    if identifier_name(&outer_member.object)? != internal_name {
        return None;
    }
    let Expression::AssignmentExpression(inner_assignment) =
        without_parentheses(&outer_member.expression)
    else {
        return None;
    };
    if inner_assignment.operator != AssignmentOperator::Assign {
        return None;
    }

    let (object_name, key) = assignment_member_key(&inner_assignment.left)?;
    if object_name != internal_name || key.name()? != right_name {
        return None;
    }

    let (forward_key, forward_computed) = key.into_property_key(ast)?;
    let forward_property = object_property(
        ast,
        forward_key,
        inner_assignment.right.clone_in(ast.allocator),
        forward_computed,
    );

    let (reverse_key, reverse_computed) =
        expression_as_property_key(&inner_assignment.right, ast, true)?;
    let reverse_property = object_property(
        ast,
        reverse_key,
        assignment.right.clone_in(ast.allocator),
        reverse_computed,
    );

    Some((forward_property, reverse_property))
}

enum MemberKey<'b> {
    Identifier(&'b str),
    StringLiteral(&'b str),
    AssignmentExpression,
}

impl<'b> MemberKey<'b> {
    fn name(&self) -> Option<&'b str> {
        match self {
            Self::Identifier(name) | Self::StringLiteral(name) => Some(name),
            Self::AssignmentExpression => None,
        }
    }

    fn is_assignment_expression(&self) -> bool {
        matches!(self, Self::AssignmentExpression)
    }

    fn into_property_key<'a>(self, ast: AstBuilder<'a>) -> Option<(PropertyKey<'a>, bool)> {
        match self {
            Self::Identifier(name) => Some((
                ast.property_key_static_identifier(Span::default(), ast.ident(name)),
                false,
            )),
            Self::StringLiteral(name) if is_valid_identifier_name(name) => Some((
                ast.property_key_static_identifier(Span::default(), ast.ident(name)),
                false,
            )),
            Self::StringLiteral(name) => Some((
                PropertyKey::StringLiteral(ast.alloc_string_literal(
                    Span::default(),
                    ast.str(name),
                    None,
                )),
                false,
            )),
            Self::AssignmentExpression => None,
        }
    }
}

fn assignment_member_key<'b, 'a>(
    target: &'b AssignmentTarget<'a>,
) -> Option<(&'b str, MemberKey<'b>)> {
    match target {
        AssignmentTarget::StaticMemberExpression(member) => Some((
            identifier_name(&member.object)?,
            MemberKey::Identifier(member.property.name.as_str()),
        )),
        AssignmentTarget::ComputedMemberExpression(member) => {
            let object_name = identifier_name(&member.object)?;
            let key = match without_parentheses(&member.expression) {
                Expression::StringLiteral(literal) => {
                    MemberKey::StringLiteral(literal.value.as_str())
                }
                Expression::AssignmentExpression(_) => MemberKey::AssignmentExpression,
                _ => return None,
            };
            Some((object_name, key))
        }
        _ => None,
    }
}

fn expression_as_property_key<'a>(
    expression: &Expression<'a>,
    ast: AstBuilder<'a>,
    computed_for_expression: bool,
) -> Option<(PropertyKey<'a>, bool)> {
    match expression {
        Expression::NumericLiteral(literal) => Some((
            PropertyKey::NumericLiteral(literal.clone_in(ast.allocator)),
            false,
        )),
        Expression::StringLiteral(literal) => Some((
            PropertyKey::StringLiteral(literal.clone_in(ast.allocator)),
            false,
        )),
        Expression::Identifier(identifier) => Some((
            PropertyKey::Identifier(identifier.clone_in(ast.allocator)),
            computed_for_expression,
        )),
        Expression::UnaryExpression(unary) => Some((
            PropertyKey::UnaryExpression(unary.clone_in(ast.allocator)),
            true,
        )),
        Expression::StaticMemberExpression(member) => Some((
            PropertyKey::StaticMemberExpression(member.clone_in(ast.allocator)),
            true,
        )),
        Expression::ComputedMemberExpression(member) => Some((
            PropertyKey::ComputedMemberExpression(member.clone_in(ast.allocator)),
            true,
        )),
        _ => None,
    }
}

fn object_property<'a>(
    ast: AstBuilder<'a>,
    key: PropertyKey<'a>,
    value: Expression<'a>,
    computed: bool,
) -> ObjectPropertyKind<'a> {
    ast.object_property_kind_object_property(
        Span::default(),
        PropertyKind::Init,
        key,
        value,
        false,
        false,
        computed,
    )
}

fn enum_spread_property<'a>(
    ast: AstBuilder<'a>,
    enum_name: &str,
    synthetic_replacements: &mut Vec<SyntheticTrailingComment>,
) -> ObjectPropertyKind<'a> {
    let empty_object = ast.expression_object(Span::default(), ast.vec());
    let fallback = ast.expression_logical(
        Span::default(),
        ast.expression_identifier(Span::default(), ast.ident(enum_name)),
        LogicalOperator::Or,
        empty_object,
    );
    synthetic_replacements.push(SyntheticTrailingComment {
        candidates: vec![format!("...{enum_name} || {{}}")],
        replacement: format!("...({enum_name} || {{}})"),
    });
    ast.object_property_kind_spread_property(
        Span::default(),
        ast.expression_parenthesized(Span::default(), fallback),
    )
}

fn assignment_statement<'a>(
    ast: AstBuilder<'a>,
    enum_name: &str,
    object: Expression<'a>,
) -> Statement<'a> {
    let target = AssignmentTarget::AssignmentTargetIdentifier(
        ast.alloc_identifier_reference(Span::default(), ast.ident(enum_name)),
    );
    let expression =
        ast.expression_assignment(Span::default(), AssignmentOperator::Assign, target, object);
    ast.statement_expression(Span::default(), expression)
}

fn assignment_target_identifier_name<'b, 'a>(target: &'b AssignmentTarget<'a>) -> Option<&'b str> {
    match target {
        AssignmentTarget::AssignmentTargetIdentifier(identifier) => Some(identifier.name.as_str()),
        _ => None,
    }
}

fn identifier_name<'b, 'a>(expression: &'b Expression<'a>) -> Option<&'b str> {
    match without_parentheses(expression) {
        Expression::Identifier(identifier) => Some(identifier.name.as_str()),
        _ => None,
    }
}

fn string_literal_value<'b, 'a>(expression: &'b Expression<'a>) -> Option<&'b str> {
    match without_parentheses(expression) {
        Expression::StringLiteral(literal) => Some(literal.value.as_str()),
        _ => None,
    }
}

fn without_parentheses<'b, 'a>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            without_parentheses(&parenthesized.expression)
        }
        expression => expression,
    }
}

fn is_valid_identifier_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    (first == '_' || first == '$' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn restores_numeric_enum_iife() {
        define_ast_inline_test(transform_ast)(
            "
var Direction;
(function (Direction) {
  Direction[Direction[\"Up\"] = 1] = \"Up\";
  Direction[Direction[\"Down\"] = 2] = \"Down\";
  Direction[Direction[\"Right\"] = -4] = \"Right\";
})(Direction || (Direction = {}));
",
            "
var Direction = {
  Up: 1,
  Down: 2,
  Right: -4,
  1: \"Up\",
  2: \"Down\",
  [-4]: \"Right\"
};
",
        );
    }

    #[test]
    fn restores_string_enum_iife() {
        define_ast_inline_test(transform_ast)(
            "
var Direction;
(function (Direction) {
  Direction[\"Up\"] = \"UP\";
  Direction[\"Down\"] = \"DOWN\";
  Direction.Left = \"LEFT\";
  Direction.Right = \"RIGHT\";
})(Direction || (Direction = {}));
",
            "
var Direction = {
  Up: \"UP\",
  Down: \"DOWN\",
  Left: \"LEFT\",
  Right: \"RIGHT\"
};
",
        );
    }

    #[test]
    fn handles_mangled_and_invalid_enum_keys() {
        define_ast_inline_test(transform_ast)(
            "
var RenderMode;
(function (i) {
  i[i[\"2D\"] = 1] = \"2D\";
  i[i[\"WebGL\"] = 2] = \"WebGL\";
})(RenderMode || (RenderMode = {}));
",
            "
var RenderMode = {
  \"2D\": 1,
  WebGL: 2,
  1: \"2D\",
  2: \"WebGL\"
};
",
        );
    }

    #[test]
    fn handles_declaration_merging() {
        define_ast_inline_test(transform_ast)(
            "
var Direction;
(function (Direction) {
  Direction[Direction[\"Up\"] = -1] = \"Up\";
  Direction[\"Down\"] = \"DOWN\";
})(Direction || (Direction = {}));
(function (Direction) {
  Direction[\"Left\"] = \"LEFT\";
  Direction[\"Right\"] = \"RIGHT\";
})(Direction || (Direction = {}));
",
            "
var Direction = {
  Up: -1,
  Down: \"DOWN\",
  [-1]: \"Up\"
};
Direction = {
  ...(Direction || {}),
  Left: \"LEFT\",
  Right: \"RIGHT\"
};
",
        );
    }

    #[test]
    fn handles_terser_and_esbuild_forms() {
        define_ast_inline_test(transform_ast)(
            "
var o;
!function(o){
  o[o.Up=1]=\"Up\";
  o[\"Down\"]=\"DOWN\";
}(o || (o = {}));

var Direction = ((m) => {
  m[(m.Up = 1)] = \"Up\";
  m.Down = \"DOWN\";
  return m;
})(Direction || {});
var Direction = ((m) => {
  m.Left = \"LEFT\";
  m.Right = \"RIGHT\";
  return m;
})(Direction || {});
",
            "
var o = {
  Up: 1,
  Down: \"DOWN\",
  1: \"Up\"
};
var Direction = {
  Up: 1,
  Down: \"DOWN\",
  1: \"Up\"
};
var Direction = {
  ...(Direction || {}),
  Left: \"LEFT\",
  Right: \"RIGHT\"
};
",
        );
    }

    #[test]
    fn handles_swc_compressed_assignment_form() {
        define_ast_inline_test(transform_ast)(
            "
var Direction;
var Direction1;
Direction1 = Direction || (Direction = {});
Direction1[Direction1.Up = 1] = \"Up\";
Direction1.Down = \"DOWN\";
",
            "
var Direction = {
  Up: 1,
  Down: \"DOWN\",
  1: \"Up\"
};
",
        );
    }
}
