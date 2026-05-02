use std::collections::{HashMap, HashSet, VecDeque};

use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{
        Argument, AssignmentTarget, BindingPattern, Expression, ForStatementInit, ForStatementLeft,
        ImportDeclaration, ImportDeclarationSpecifier, Program, Statement, VariableDeclaration,
        VariableDeclarationKind, VariableDeclarator,
    },
    AstBuilder,
};
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, Span};
use oxc_syntax::operator::AssignmentOperator;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

use crate::utils::is_helper_function_call::is_helper_callee;

const MODULE_NAME: &str = "@babel/runtime/helpers/createForOfIteratorHelper";
const MODULE_ESM_NAME: &str = "@babel/runtime/helpers/esm/createForOfIteratorHelper";
const LOOSE_MODULE_NAME: &str = "@babel/runtime/helpers/createForOfIteratorHelperLoose";
const LOOSE_MODULE_ESM_NAME: &str = "@babel/runtime/helpers/esm/createForOfIteratorHelperLoose";
const HELPER_SOURCES: &[&str] = &[
    MODULE_NAME,
    MODULE_ESM_NAME,
    LOOSE_MODULE_NAME,
    LOOSE_MODULE_ESM_NAME,
];

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let helper_locals = find_helper_locals(&source.program);
    if helper_locals.is_empty() {
        return Ok(());
    }

    let reference_counts = helper_reference_counts(&source.program, &helper_locals);
    let mut restorer = CreateForOfIteratorHelperRestorer {
        ast: AstBuilder::new(source.allocator),
        helper_locals,
        processed_counts: HashMap::new(),
    };

    restorer.transform_statement_list(&mut source.program.body);

    let removable_helpers = restorer
        .processed_counts
        .iter()
        .filter_map(|(helper, processed)| {
            (reference_counts.get(helper).copied().unwrap_or_default() == *processed)
                .then(|| helper.clone())
        })
        .collect::<HashSet<_>>();

    if !removable_helpers.is_empty() {
        remove_helper_declarations(
            &mut source.program,
            &removable_helpers,
            AstBuilder::new(source.allocator),
        );
    }

    Ok(())
}

struct CreateForOfIteratorHelperRestorer<'a> {
    ast: AstBuilder<'a>,
    helper_locals: Vec<String>,
    processed_counts: HashMap<String, usize>,
}

impl<'a> CreateForOfIteratorHelperRestorer<'a> {
    fn transform_statement_list(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        let old_statements = statements.take_in(self.ast);
        let mut queue = VecDeque::from_iter(old_statements);
        let mut new_statements = self.ast.vec();

        while let Some(statement) = queue.pop_front() {
            match statement {
                Statement::VariableDeclaration(declaration) => {
                    if self.match_helper_declarator(&declaration).is_none() {
                        new_statements.push(Statement::VariableDeclaration(declaration));
                        continue;
                    }

                    let Some(first_next_statement) = queue.pop_front() else {
                        new_statements.push(Statement::VariableDeclaration(declaration));
                        continue;
                    };

                    let (step_declaration, try_statement) = match first_next_statement {
                        Statement::TryStatement(try_statement) => (None, try_statement),
                        Statement::VariableDeclaration(step_declaration) => {
                            let Some(second_next_statement) = queue.pop_front() else {
                                new_statements.push(Statement::VariableDeclaration(declaration));
                                new_statements
                                    .push(Statement::VariableDeclaration(step_declaration));
                                continue;
                            };
                            let Statement::TryStatement(try_statement) = second_next_statement
                            else {
                                new_statements.push(Statement::VariableDeclaration(declaration));
                                new_statements
                                    .push(Statement::VariableDeclaration(step_declaration));
                                new_statements.push(second_next_statement);
                                continue;
                            };
                            (Some(step_declaration), try_statement)
                        }
                        next_statement => {
                            new_statements.push(Statement::VariableDeclaration(declaration));
                            new_statements.push(next_statement);
                            continue;
                        }
                    };

                    match self.restore_standard_for_of(declaration, step_declaration, try_statement)
                    {
                        Ok((kept_declarations, for_of_statement)) => {
                            new_statements.extend(kept_declarations);
                            new_statements.push(for_of_statement);
                        }
                        Err((declaration, step_declaration, try_statement)) => {
                            new_statements.push(Statement::VariableDeclaration(declaration));
                            if let Some(step_declaration) = step_declaration {
                                new_statements
                                    .push(Statement::VariableDeclaration(step_declaration));
                            }
                            new_statements.push(Statement::TryStatement(try_statement));
                        }
                    }
                }
                Statement::ForStatement(for_statement) => {
                    new_statements.push(match self.restore_loose_for_of(for_statement) {
                        Ok(for_of_statement) => for_of_statement,
                        Err(for_statement) => Statement::ForStatement(for_statement),
                    });
                }
                statement => new_statements.push(statement),
            }
        }

        *statements = new_statements;
    }

    fn restore_standard_for_of(
        &mut self,
        mut declaration: oxc_allocator::Box<'a, VariableDeclaration<'a>>,
        step_declaration: Option<oxc_allocator::Box<'a, VariableDeclaration<'a>>>,
        mut try_statement: oxc_allocator::Box<'a, oxc_ast::ast::TryStatement<'a>>,
    ) -> std::result::Result<
        (oxc_allocator::Vec<'a, Statement<'a>>, Statement<'a>),
        (
            oxc_allocator::Box<'a, VariableDeclaration<'a>>,
            Option<oxc_allocator::Box<'a, VariableDeclaration<'a>>>,
            oxc_allocator::Box<'a, oxc_ast::ast::TryStatement<'a>>,
        ),
    > {
        let Some(helper_match) = self.match_helper_declarator(&declaration) else {
            return Err((declaration, step_declaration, try_statement));
        };
        let Some(for_index) = find_for_statement_index(&try_statement.block.body) else {
            return Err((declaration, step_declaration, try_statement));
        };
        let Statement::ForStatement(for_statement) = &try_statement.block.body[for_index] else {
            return Err((declaration, step_declaration, try_statement));
        };
        let Some(test) = &for_statement.test else {
            return Err((declaration, step_declaration, try_statement));
        };
        let Some(step_name) = find_step_assignment(test, &helper_match.iterator_name, false) else {
            return Err((declaration, step_declaration, try_statement));
        };
        let body_plan =
            if let Some(result_plan) = match_result_plan(&for_statement.body, &step_name) {
                BodyPlan::Direct(result_plan)
            } else if let Some(loop_plan) = match_loop_function_body_plan(
                &try_statement.block.body,
                for_index,
                for_statement,
                &step_name,
            ) {
                BodyPlan::LoopFunction(loop_plan)
            } else {
                return Err((declaration, step_declaration, try_statement));
            };

        let object = self.take_helper_object(&mut declaration, helper_match.helper_index);
        let Some(object) = object else {
            return Err((declaration, step_declaration, try_statement));
        };
        let kept_declaration =
            self.remove_iterator_declarators(declaration, &helper_match.iterator_name, &step_name);
        let kept_step_declaration = step_declaration.and_then(|declaration| {
            self.remove_iterator_declarators(declaration, &helper_match.iterator_name, &step_name)
        });

        let rebuilt = match body_plan {
            BodyPlan::Direct(result_plan) => {
                let Statement::ForStatement(mut for_statement) = try_statement
                    .block
                    .body
                    .take_in(self.ast)
                    .into_iter()
                    .nth(for_index)
                    .expect("validated for statement index")
                else {
                    return Err((
                        kept_declaration.unwrap_or_else(|| {
                            self.ast.alloc_variable_declaration(
                                Span::default(),
                                VariableDeclarationKind::Var,
                                self.ast.vec(),
                                false,
                            )
                        }),
                        kept_step_declaration,
                        try_statement,
                    ));
                };

                self.rebuild_for_body(&mut for_statement.body, result_plan)
            }
            BodyPlan::LoopFunction(loop_plan) => {
                self.rebuild_loop_function_body(&mut try_statement, loop_plan)
            }
        };

        let Some((left, body, body_span)) = rebuilt else {
            return Err((
                kept_declaration.unwrap_or_else(|| {
                    self.ast.alloc_variable_declaration(
                        Span::default(),
                        VariableDeclarationKind::Var,
                        self.ast.vec(),
                        false,
                    )
                }),
                kept_step_declaration,
                try_statement,
            ));
        };

        *self
            .processed_counts
            .entry(helper_match.helper_local)
            .or_default() += 1;

        let for_of_statement = self.ast.statement_for_of(
            try_statement.span,
            false,
            left,
            object,
            self.ast.statement_block(body_span, body),
        );
        let mut kept_declarations = self.ast.vec();
        if let Some(declaration) = kept_declaration {
            kept_declarations.push(Statement::VariableDeclaration(declaration));
        }
        if let Some(declaration) = kept_step_declaration {
            kept_declarations.push(Statement::VariableDeclaration(declaration));
        }

        Ok((kept_declarations, for_of_statement))
    }

    fn restore_loose_for_of(
        &mut self,
        mut for_statement: oxc_allocator::Box<'a, oxc_ast::ast::ForStatement<'a>>,
    ) -> std::result::Result<Statement<'a>, oxc_allocator::Box<'a, oxc_ast::ast::ForStatement<'a>>>
    {
        let Some(ForStatementInit::VariableDeclaration(declaration)) = &for_statement.init else {
            return Err(for_statement);
        };
        let Some(helper_match) = self.match_helper_declarator(declaration) else {
            return Err(for_statement);
        };
        let Some(test) = &for_statement.test else {
            return Err(for_statement);
        };
        let Some(step_name) = find_step_assignment(test, &helper_match.iterator_name, true) else {
            return Err(for_statement);
        };
        let Some(result_plan) = match_result_plan(&for_statement.body, &step_name) else {
            return Err(for_statement);
        };

        let Some(ForStatementInit::VariableDeclaration(mut declaration)) =
            for_statement.init.take()
        else {
            return Err(for_statement);
        };
        let Some(object) = self.take_helper_object(&mut declaration, helper_match.helper_index)
        else {
            return Err(for_statement);
        };
        let Some((left, body, _body_span)) =
            self.rebuild_for_body(&mut for_statement.body, result_plan)
        else {
            return Err(for_statement);
        };

        *self
            .processed_counts
            .entry(helper_match.helper_local)
            .or_default() += 1;

        Ok(self.ast.statement_for_of(
            for_statement.span,
            false,
            left,
            object,
            self.ast.statement_block(for_statement.body.span(), body),
        ))
    }

    fn match_helper_declarator(
        &self,
        declaration: &VariableDeclaration<'a>,
    ) -> Option<HelperDeclaratorMatch> {
        declaration
            .declarations
            .iter()
            .enumerate()
            .find_map(|(helper_index, declarator)| {
                let BindingPattern::BindingIdentifier(iterator) = &declarator.id else {
                    return None;
                };
                let Expression::CallExpression(call) = declarator.init.as_ref()? else {
                    return None;
                };
                if call.arguments.is_empty()
                    || call.arguments.len() > 2
                    || matches!(call.arguments.first(), Some(Argument::SpreadElement(_)))
                {
                    return None;
                }

                self.helper_locals
                    .iter()
                    .find(|helper| is_helper_callee(&call.callee, helper))
                    .map(|helper_local| HelperDeclaratorMatch {
                        helper_index,
                        helper_local: helper_local.clone(),
                        iterator_name: iterator.name.as_str().to_string(),
                    })
            })
    }

    fn take_helper_object(
        &self,
        declaration: &mut VariableDeclaration<'a>,
        helper_index: usize,
    ) -> Option<Expression<'a>> {
        let declarator = declaration.declarations.get_mut(helper_index)?;
        let Expression::CallExpression(call) = declarator.init.take()? else {
            return None;
        };
        call.unbox()
            .arguments
            .into_iter()
            .next()
            .and_then(argument_to_expression)
    }

    fn remove_iterator_declarators(
        &self,
        mut declaration: oxc_allocator::Box<'a, VariableDeclaration<'a>>,
        iterator_name: &str,
        step_name: &str,
    ) -> Option<oxc_allocator::Box<'a, VariableDeclaration<'a>>> {
        let old_declarations = declaration.declarations.take_in(self.ast);
        let mut kept_declarations = self.ast.vec();

        for declarator in old_declarations {
            if binding_identifier_name(&declarator)
                .is_some_and(|name| name == iterator_name || name == step_name)
            {
                continue;
            }

            kept_declarations.push(declarator);
        }

        if kept_declarations.is_empty() {
            None
        } else {
            declaration.declarations = kept_declarations;
            Some(declaration)
        }
    }

    fn rebuild_for_body(
        &self,
        body: &mut Statement<'a>,
        result_plan: ResultPlan,
    ) -> Option<(
        ForStatementLeft<'a>,
        oxc_allocator::Vec<'a, Statement<'a>>,
        Span,
    )> {
        let Statement::BlockStatement(block) = body else {
            return None;
        };

        let body_span = block.span;
        let old_body = block.body.take_in(self.ast);
        let (left, body) = self.rebuild_statements(old_body, result_plan)?;
        Some((left, body, body_span))
    }

    fn rebuild_loop_function_body(
        &self,
        try_statement: &mut oxc_allocator::Box<'a, oxc_ast::ast::TryStatement<'a>>,
        loop_plan: LoopFunctionPlan,
    ) -> Option<(
        ForStatementLeft<'a>,
        oxc_allocator::Vec<'a, Statement<'a>>,
        Span,
    )> {
        let old_try_body = try_statement.block.body.take_in(self.ast);

        for (statement_index, statement) in old_try_body.into_iter().enumerate() {
            if statement_index != loop_plan.statement_index {
                continue;
            }

            let Statement::VariableDeclaration(mut declaration) = statement else {
                return None;
            };
            let old_declarations = declaration.declarations.take_in(self.ast);

            for mut declarator in old_declarations {
                if binding_identifier_name(&declarator) != Some(loop_plan.loop_name.as_str()) {
                    continue;
                }

                let Some(Expression::FunctionExpression(mut function)) = declarator.init.take()
                else {
                    return None;
                };
                let body = function.body.as_mut()?;
                let body_span = body.span;
                let old_body = body.statements.take_in(self.ast);
                let (left, body) = self.rebuild_statements(old_body, loop_plan.result_plan)?;
                return Some((left, body, body_span));
            }

            return None;
        }

        None
    }

    fn rebuild_statements(
        &self,
        old_body: oxc_allocator::Vec<'a, Statement<'a>>,
        result_plan: ResultPlan,
    ) -> Option<(ForStatementLeft<'a>, oxc_allocator::Vec<'a, Statement<'a>>)> {
        let mut new_body = self.ast.vec_with_capacity(old_body.len());
        let mut left = None;
        let mut prepend = None;

        for (statement_index, statement) in old_body.into_iter().enumerate() {
            match (&result_plan, statement_index, statement) {
                (
                    ResultPlan::VariableDeclarator {
                        statement_index,
                        declarator_index,
                    },
                    current_index,
                    Statement::VariableDeclaration(mut declaration),
                ) if *statement_index == current_index => {
                    let old_declarations = declaration.declarations.take_in(self.ast);
                    let mut kept_declarations = self.ast.vec();

                    for (current_declarator_index, mut declarator) in
                        old_declarations.into_iter().enumerate()
                    {
                        if *declarator_index == current_declarator_index {
                            declarator.init = None;
                            let mut declarations = self.ast.vec_with_capacity(1);
                            declarations.push(declarator);
                            left = Some(self.ast.for_statement_left_variable_declaration(
                                declaration.span,
                                declaration.kind,
                                declarations,
                                declaration.declare,
                            ));
                        } else {
                            kept_declarations.push(declarator);
                        }
                    }

                    if !kept_declarations.is_empty() {
                        declaration.declarations = kept_declarations;
                        new_body.push(Statement::VariableDeclaration(declaration));
                    }
                }
                (
                    ResultPlan::Assignment { statement_index },
                    current_index,
                    Statement::ExpressionStatement(expression_statement),
                ) if *statement_index == current_index => {
                    let Expression::AssignmentExpression(assignment) =
                        expression_statement.unbox().expression
                    else {
                        return None;
                    };
                    let assignment = assignment.unbox();
                    let target_expression = assignment_target_to_expression(assignment.left)?;
                    let span = target_expression.span();
                    left = Some(ForStatementLeft::AssignmentTargetIdentifier(
                        self.ast.alloc_identifier_reference(span, "value"),
                    ));
                    let assignment_target = AssignmentTarget::AssignmentTargetIdentifier(
                        self.ast.alloc_identifier_reference(span, "value"),
                    );
                    prepend = Some(self.ast.statement_expression(
                        span,
                        self.ast.expression_assignment(
                            span,
                            AssignmentOperator::Assign,
                            assignment_target,
                            target_expression,
                        ),
                    ));
                }
                (_, _, statement) => new_body.push(statement),
            }
        }

        if let Some(statement) = prepend {
            new_body.insert(0, statement);
        }

        Some((left?, new_body))
    }
}

struct HelperDeclaratorMatch {
    helper_index: usize,
    helper_local: String,
    iterator_name: String,
}

enum ResultPlan {
    VariableDeclarator {
        statement_index: usize,
        declarator_index: usize,
    },
    Assignment {
        statement_index: usize,
    },
}

enum BodyPlan {
    Direct(ResultPlan),
    LoopFunction(LoopFunctionPlan),
}

struct LoopFunctionPlan {
    statement_index: usize,
    loop_name: String,
    result_plan: ResultPlan,
}

fn find_for_statement_index(statements: &oxc_allocator::Vec<Statement>) -> Option<usize> {
    statements
        .iter()
        .position(|statement| matches!(statement, Statement::ForStatement(_)))
}

fn find_step_assignment(
    expression: &Expression,
    iterator_name: &str,
    loose: bool,
) -> Option<String> {
    match expression {
        Expression::AssignmentExpression(assignment) => {
            if assignment.operator != AssignmentOperator::Assign {
                return None;
            }
            let AssignmentTarget::AssignmentTargetIdentifier(step) = &assignment.left else {
                return None;
            };
            if is_iterator_next_call(&assignment.right, iterator_name, loose) {
                Some(step.name.as_str().to_string())
            } else {
                None
            }
        }
        Expression::UnaryExpression(unary) => {
            find_step_assignment(&unary.argument, iterator_name, loose)
        }
        Expression::StaticMemberExpression(member) => {
            find_step_assignment(&member.object, iterator_name, loose)
        }
        Expression::ParenthesizedExpression(parenthesized) => {
            find_step_assignment(&parenthesized.expression, iterator_name, loose)
        }
        _ => None,
    }
}

fn is_iterator_next_call(expression: &Expression, iterator_name: &str, loose: bool) -> bool {
    let Expression::CallExpression(call) = expression else {
        return false;
    };
    if !call.arguments.is_empty() {
        return false;
    }

    if loose {
        return matches!(&call.callee, Expression::Identifier(identifier) if identifier.name.as_str() == iterator_name);
    }

    matches!(
        &call.callee,
        Expression::StaticMemberExpression(member)
            if is_identifier(&member.object, iterator_name)
                && member.property.name.as_str() == "n"
    )
}

fn match_result_plan(body: &Statement, step_name: &str) -> Option<ResultPlan> {
    let Statement::BlockStatement(block) = body else {
        return None;
    };

    match_result_plan_in_statements(&block.body, step_name)
}

fn match_result_plan_in_statements(
    statements: &oxc_allocator::Vec<Statement>,
    step_name: &str,
) -> Option<ResultPlan> {
    let mut result_plan = None;
    for (statement_index, statement) in statements.iter().enumerate() {
        match statement {
            Statement::VariableDeclaration(declaration) => {
                for (declarator_index, declarator) in declaration.declarations.iter().enumerate() {
                    if matches!(declarator.id, BindingPattern::BindingIdentifier(_))
                        && declarator
                            .init
                            .as_ref()
                            .is_some_and(|init| is_step_value_member(init, step_name))
                    {
                        if result_plan.is_some() {
                            return None;
                        }
                        result_plan = Some(ResultPlan::VariableDeclarator {
                            statement_index,
                            declarator_index,
                        });
                    }
                }
            }
            Statement::ExpressionStatement(expression_statement) => {
                if let Expression::AssignmentExpression(assignment) =
                    &expression_statement.expression
                {
                    if assignment.operator == AssignmentOperator::Assign
                        && is_step_value_member(&assignment.right, step_name)
                    {
                        if result_plan.is_some() {
                            return None;
                        }
                        result_plan = Some(ResultPlan::Assignment { statement_index });
                    }
                }
            }
            _ => {}
        }
    }

    result_plan
}

fn match_loop_function_body_plan(
    statements: &oxc_allocator::Vec<Statement>,
    for_index: usize,
    for_statement: &oxc_ast::ast::ForStatement,
    step_name: &str,
) -> Option<LoopFunctionPlan> {
    let loop_name = loop_call_name(&for_statement.body)?.to_string();

    for (statement_index, statement) in statements.iter().take(for_index).enumerate().rev() {
        if let Some(function_body) = loop_function_body(statement, &loop_name) {
            let result_plan =
                match_result_plan_in_statements(&function_body.statements, step_name)?;
            return Some(LoopFunctionPlan {
                statement_index,
                loop_name,
                result_plan,
            });
        }
    }

    None
}

fn loop_call_name<'a>(body: &'a Statement<'a>) -> Option<&'a str> {
    let Statement::BlockStatement(block) = body else {
        return None;
    };
    if block.body.len() != 1 {
        return None;
    }

    let Statement::ExpressionStatement(expression_statement) = &block.body[0] else {
        return None;
    };
    let Expression::CallExpression(call) = &expression_statement.expression else {
        return None;
    };
    if !call.arguments.is_empty() {
        return None;
    }
    let Expression::Identifier(callee) = &call.callee else {
        return None;
    };

    Some(callee.name.as_str())
}

fn loop_function_body<'b, 'a>(
    statement: &'b Statement<'a>,
    loop_name: &str,
) -> Option<&'b oxc_ast::ast::FunctionBody<'a>> {
    let Statement::VariableDeclaration(declaration) = statement else {
        return None;
    };

    for declarator in &declaration.declarations {
        if binding_identifier_name(declarator) != Some(loop_name) {
            continue;
        }
        let Some(Expression::FunctionExpression(function)) = &declarator.init else {
            return None;
        };
        return function.body.as_deref();
    }

    None
}

fn is_step_value_member(expression: &Expression, step_name: &str) -> bool {
    matches!(
        expression,
        Expression::StaticMemberExpression(member)
            if is_identifier(&member.object, step_name)
                && member.property.name.as_str() == "value"
    )
}

fn is_identifier(expression: &Expression, name: &str) -> bool {
    matches!(expression, Expression::Identifier(identifier) if identifier.name.as_str() == name)
}

fn binding_identifier_name<'a>(declarator: &'a VariableDeclarator<'a>) -> Option<&'a str> {
    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return None;
    };

    Some(identifier.name.as_str())
}

fn assignment_target_to_expression<'a>(target: AssignmentTarget<'a>) -> Option<Expression<'a>> {
    match target {
        AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
            Some(Expression::Identifier(identifier))
        }
        AssignmentTarget::ComputedMemberExpression(member) => {
            Some(Expression::ComputedMemberExpression(member))
        }
        AssignmentTarget::StaticMemberExpression(member) => {
            Some(Expression::StaticMemberExpression(member))
        }
        AssignmentTarget::PrivateFieldExpression(member) => {
            Some(Expression::PrivateFieldExpression(member))
        }
        _ => None,
    }
}

fn find_helper_locals(program: &Program) -> Vec<String> {
    let mut locals = Vec::new();

    for statement in &program.body {
        match statement {
            Statement::ImportDeclaration(import)
                if is_helper_source(import.source.value.as_str()) =>
            {
                collect_import_locals(import, &mut locals);
            }
            Statement::VariableDeclaration(declaration) => {
                collect_require_locals(declaration, &mut locals);
            }
            _ => {}
        }
    }

    locals
}

fn collect_import_locals(import: &ImportDeclaration, locals: &mut Vec<String>) {
    let Some(specifiers) = &import.specifiers else {
        return;
    };

    for specifier in specifiers {
        match specifier {
            ImportDeclarationSpecifier::ImportDefaultSpecifier(default) => {
                locals.push(default.local.name.as_str().to_string());
            }
            ImportDeclarationSpecifier::ImportSpecifier(named) => {
                locals.push(named.local.name.as_str().to_string());
            }
            ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace) => {
                locals.push(namespace.local.name.as_str().to_string());
            }
        }
    }
}

fn collect_require_locals(declaration: &VariableDeclaration, locals: &mut Vec<String>) {
    for declarator in &declaration.declarations {
        if !is_helper_require_declarator(declarator) {
            continue;
        }

        let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            continue;
        };

        locals.push(identifier.name.as_str().to_string());
    }
}

fn helper_reference_counts(program: &Program, helper_locals: &[String]) -> HashMap<String, usize> {
    let semantic = SemanticBuilder::new().build(program).semantic;
    let scoping = semantic.scoping();
    helper_locals
        .iter()
        .filter_map(|helper| {
            scoping
                .get_root_binding(helper.as_str().into())
                .map(|symbol_id| {
                    (
                        helper.clone(),
                        scoping.get_resolved_reference_ids(symbol_id).len(),
                    )
                })
        })
        .collect()
}

fn remove_helper_declarations<'a>(
    program: &mut Program<'a>,
    removable_helpers: &HashSet<String>,
    ast: AstBuilder<'a>,
) {
    let old_body = program.body.take_in(ast);
    let mut new_body = ast.vec_with_capacity(old_body.len());

    for statement in old_body {
        match statement {
            Statement::ImportDeclaration(import)
                if is_helper_source(import.source.value.as_str()) =>
            {
                if let Some(statement) = remove_import_helpers(import, removable_helpers) {
                    new_body.push(statement);
                }
            }
            Statement::VariableDeclaration(declaration) => {
                if let Some(statement) = remove_require_helpers(declaration, removable_helpers, ast)
                {
                    new_body.push(statement);
                }
            }
            statement => new_body.push(statement),
        }
    }

    program.body = new_body;
}

fn remove_import_helpers<'a>(
    mut import: oxc_allocator::Box<'a, ImportDeclaration<'a>>,
    removable_helpers: &HashSet<String>,
) -> Option<Statement<'a>> {
    let Some(specifiers) = &mut import.specifiers else {
        return Some(Statement::ImportDeclaration(import));
    };

    specifiers.retain(|specifier| !import_specifier_is_removable(specifier, removable_helpers));

    if specifiers.is_empty() {
        None
    } else {
        Some(Statement::ImportDeclaration(import))
    }
}

fn remove_require_helpers<'a>(
    mut declaration: oxc_allocator::Box<'a, VariableDeclaration<'a>>,
    removable_helpers: &HashSet<String>,
    ast: AstBuilder<'a>,
) -> Option<Statement<'a>> {
    let old_declarations = declaration.declarations.take_in(ast);
    let mut kept_declarations = ast.vec();

    for declarator in old_declarations {
        if require_declarator_is_removable(&declarator, removable_helpers) {
            continue;
        }

        kept_declarations.push(declarator);
    }

    if kept_declarations.is_empty() {
        None
    } else {
        declaration.declarations = kept_declarations;
        Some(Statement::VariableDeclaration(declaration))
    }
}

fn import_specifier_is_removable(
    specifier: &ImportDeclarationSpecifier,
    removable_helpers: &HashSet<String>,
) -> bool {
    match specifier {
        ImportDeclarationSpecifier::ImportDefaultSpecifier(default) => {
            removable_helpers.contains(default.local.name.as_str())
        }
        ImportDeclarationSpecifier::ImportSpecifier(named) => {
            removable_helpers.contains(named.local.name.as_str())
        }
        ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace) => {
            removable_helpers.contains(namespace.local.name.as_str())
        }
    }
}

fn require_declarator_is_removable(
    declarator: &VariableDeclarator,
    removable_helpers: &HashSet<String>,
) -> bool {
    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return false;
    };

    removable_helpers.contains(identifier.name.as_str()) && is_helper_require_declarator(declarator)
}

fn is_helper_require_declarator(declarator: &VariableDeclarator) -> bool {
    let Some(init) = &declarator.init else {
        return false;
    };

    require_source(init).is_some_and(is_helper_source)
}

fn require_source<'a>(expression: &'a Expression<'a>) -> Option<&'a str> {
    let Expression::CallExpression(call) = expression else {
        return None;
    };

    if !matches!(&call.callee, Expression::Identifier(identifier) if identifier.name.as_str() == "require")
        || call.arguments.len() != 1
    {
        return None;
    }

    let Some(Argument::StringLiteral(source)) = call.arguments.first() else {
        return None;
    };

    Some(source.value.as_str())
}

fn is_helper_source(source: &str) -> bool {
    HELPER_SOURCES.contains(&source)
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
    fn restores_standard_create_for_of_iterator_helper() {
        define_ast_inline_test(transform_ast)(
            r#"
var _createForOfIteratorHelper = require("@babel/runtime/helpers/createForOfIteratorHelper");

var _iterator = _createForOfIteratorHelper(arr), _step;
try {
  for (_iterator.s(); !(_step = _iterator.n()).done;) {
    var _result = _step.value;
  }
} catch (err) {
  _iterator.e(err);
} finally {
  _iterator.f();
}
"#,
            "
for (var _result of arr) {}
",
        );
    }

    #[test]
    fn extracts_standard_loop_function_body() {
        define_ast_inline_test(transform_ast)(
            r#"
var _createForOfIteratorHelper = require("@babel/runtime/helpers/createForOfIteratorHelper");

var _iterator = _createForOfIteratorHelper(arr), _step;
try {
  var _loop = function _loop() {
    var _result = _step.value;
    var a = _result[0];
    a = 1;
    (function () {
      return a;
    });
  };
  for (_iterator.s(); !(_step = _iterator.n()).done;) {
    _loop();
  }
} catch (err) {
  _iterator.e(err);
} finally {
  _iterator.f();
}
"#,
            "
for (var _result of arr) {
  var a = _result[0];
  a = 1;
  (function() {
    return a;
  });
}
",
        );
    }

    #[test]
    fn restores_loose_create_for_of_iterator_helper() {
        define_ast_inline_test(transform_ast)(
            r#"
var _createForOfIteratorHelperLoose = require("@babel/runtime/helpers/createForOfIteratorHelperLoose");

var _loop = function (result) {
  result = otherValue;
  fn(() => {
    result;
  });
};
for (var _iterator = _createForOfIteratorHelperLoose(results), _step; !(_step = _iterator()).done;) {
  var result = _step.value;
  _loop(result);
}
"#,
            "
var _loop = function(result) {
  result = otherValue;
  fn(() => {
    result;
  });
};
for (var result of results) {
  _loop(result);
}
",
        );
    }

    #[test]
    fn leaves_unmatched_loop_function_wrapper_unchanged() {
        define_ast_inline_test(transform_ast)(
            r#"
var _createForOfIteratorHelper = require("@babel/runtime/helpers/createForOfIteratorHelper");

var _iterator = _createForOfIteratorHelper(arr), _step;
try {
  var _loop = function _loop() {
    var _result = other.value;
    use(_result);
  };
  for (_iterator.s(); !(_step = _iterator.n()).done;) {
    _loop();
  }
} catch (err) {
  _iterator.e(err);
} finally {
  _iterator.f();
}
"#,
            r#"
var _createForOfIteratorHelper = require("@babel/runtime/helpers/createForOfIteratorHelper");
var _iterator = _createForOfIteratorHelper(arr), _step;
try {
  var _loop = function _loop() {
    var _result = other.value;
    use(_result);
  };
  for (_iterator.s(); !(_step = _iterator.n()).done;) {
    _loop();
  }
} catch (err) {
  _iterator.e(err);
} finally {
  _iterator.f();
}
"#,
        );
    }

    #[test]
    fn restores_member_assignment_standard_helper() {
        define_ast_inline_test(transform_ast)(
            r#"
var _createForOfIteratorHelper = require("@babel/runtime/helpers/createForOfIteratorHelper");

var _iterator = _createForOfIteratorHelper(arr), _step;
try {
  for (_iterator.s(); !(_step = _iterator.n()).done;) {
    obj.prop = _step.value;
  }
} catch (err) {
  _iterator.e(err);
} finally {
  _iterator.f();
}
"#,
            "
for (value of arr) {
  value = obj.prop;
}
",
        );
    }

    #[test]
    fn leaves_unmatched_helper_scaffold_unchanged() {
        define_ast_inline_test(transform_ast)(
            r#"
var _createForOfIteratorHelper = require("@babel/runtime/helpers/createForOfIteratorHelper");

var _iterator = _createForOfIteratorHelper(arr), _step;
try {
  for (_iterator.s(); !(_step = _iterator.n()).done;) {
    var _result = other.value;
  }
} catch (err) {
  _iterator.e(err);
} finally {
  _iterator.f();
}
"#,
            r#"
var _createForOfIteratorHelper = require("@babel/runtime/helpers/createForOfIteratorHelper");
var _iterator = _createForOfIteratorHelper(arr), _step;
try {
  for (_iterator.s(); !(_step = _iterator.n()).done;) {
    var _result = other.value;
  }
} catch (err) {
  _iterator.e(err);
} finally {
  _iterator.f();
}
"#,
        );
    }

    #[test]
    fn restores_standard_helper_after_variable_merging_splits_step_declaration() {
        define_ast_inline_test(transform_ast)(
            r#"
var _createForOfIteratorHelper = require("@babel/runtime/helpers/createForOfIteratorHelper");

var _iterator = _createForOfIteratorHelper(arr);
var _step;
try {
  for (_iterator.s(); !(_step = _iterator.n()).done;) {
    var _result = _step.value;
    use(_result);
  }
} catch (err) {
  _iterator.e(err);
} finally {
  _iterator.f();
}
"#,
            "
for (var _result of arr) {
  use(_result);
}
",
        );
    }
}
