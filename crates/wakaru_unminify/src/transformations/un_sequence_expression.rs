use oxc_allocator::{CloneIn, TakeIn};
use oxc_ast::{
    ast::{
        ArrowFunctionExpression, AssignmentTarget, Expression, ForStatementInit, Statement,
        VariableDeclaration, VariableDeclarationKind, VariableDeclarator,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::{GetSpan, Span};
use oxc_syntax::operator::AssignmentOperator;
use oxc_syntax::scope::ScopeId;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut splitter = SequenceExpressionSplitter {
        ast: AstBuilder::new(source.allocator),
    };

    splitter.visit_program(&mut source.program);

    Ok(())
}

struct SequenceExpressionSplitter<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for SequenceExpressionSplitter<'a> {
    fn visit_arrow_function_expression(&mut self, arrow: &mut ArrowFunctionExpression<'a>) {
        if self.split_arrow_sequence_body(arrow) {
            walk_mut::walk_arrow_function_expression(self, arrow);
            return;
        }

        walk_mut::walk_arrow_function_expression(self, arrow);
    }

    fn visit_statement(&mut self, statement: &mut Statement<'a>) {
        walk_mut::walk_statement(self, statement);
        self.split_embedded_statement_bodies(statement);
    }

    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        walk_mut::walk_statements(self, statements);

        let old_statements = statements.take_in(self.ast);
        let mut new_statements = self.ast.vec_with_capacity(old_statements.len());

        for statement in old_statements {
            for replacement in self.split_statement(statement) {
                new_statements.push(replacement);
            }
        }

        *statements = new_statements;
    }
}

impl<'a> SequenceExpressionSplitter<'a> {
    fn split_arrow_sequence_body(&self, arrow: &mut ArrowFunctionExpression<'a>) -> bool {
        if !arrow.expression || arrow.body.statements.len() != 1 {
            return false;
        }

        let mut body = arrow.body.statements.take_in(self.ast);
        let Some(Statement::ExpressionStatement(statement)) = body.pop() else {
            arrow.body.statements = body;
            return false;
        };

        match into_sequence_expressions(statement.unbox().expression) {
            Ok(expressions) => {
                arrow.expression = false;
                arrow.body.statements = self.sequence_return_replacements(expressions);
                true
            }
            Err(expression) => {
                body.push(self.ast.statement_expression(expression.span(), expression));
                arrow.body.statements = body;
                false
            }
        }
    }

    fn split_embedded_statement_bodies(&self, statement: &mut Statement<'a>) {
        match statement {
            Statement::IfStatement(if_statement) => {
                self.split_embedded_statement(&mut if_statement.consequent);
                if let Some(alternate) = &mut if_statement.alternate {
                    self.split_embedded_statement(alternate);
                }
            }
            Statement::ForStatement(for_statement) => {
                self.split_embedded_statement(&mut for_statement.body);
            }
            Statement::ForInStatement(for_statement) => {
                self.split_embedded_statement(&mut for_statement.body);
            }
            Statement::ForOfStatement(for_statement) => {
                self.split_embedded_statement(&mut for_statement.body);
            }
            Statement::WhileStatement(while_statement) => {
                self.split_embedded_statement(&mut while_statement.body);
            }
            Statement::DoWhileStatement(do_while_statement) => {
                self.split_embedded_statement(&mut do_while_statement.body);
            }
            _ => {}
        }
    }

    fn split_embedded_statement(&self, statement: &mut Statement<'a>) {
        if matches!(statement, Statement::BlockStatement(_)) {
            return;
        }

        let old_statement = statement.take_in(self.ast);
        let mut replacements = self.split_statement(old_statement);

        if replacements.len() == 1 {
            *statement = replacements.pop().expect("one replacement");
            return;
        }

        *statement = self.block_statement(replacements);
    }

    fn split_statement(&self, statement: Statement<'a>) -> oxc_allocator::Vec<'a, Statement<'a>> {
        match statement {
            Statement::ExpressionStatement(statement) => {
                let statement = statement.unbox();
                self.split_expression_statement(statement.span, statement.expression)
            }
            Statement::ReturnStatement(mut statement) => {
                let span = statement.span;
                match statement.argument.take() {
                    Some(argument) => self.split_return_argument(span, argument),
                    None => self.single(Statement::ReturnStatement(statement)),
                }
            }
            Statement::ThrowStatement(statement) => {
                let span = statement.span;
                let argument = statement.unbox().argument;
                self.split_throw_argument(span, argument)
            }
            Statement::IfStatement(statement) => self.split_if_statement(statement),
            Statement::SwitchStatement(statement) => self.split_switch_statement(statement),
            Statement::VariableDeclaration(declaration) => {
                self.split_variable_declaration_statement(declaration)
            }
            Statement::ForStatement(statement) => self.split_for_statement(statement),
            Statement::ForInStatement(statement) => self.split_for_in_statement(statement),
            Statement::ForOfStatement(statement) => self.split_for_of_statement(statement),
            statement => self.single(statement),
        }
    }

    fn split_expression_statement(
        &self,
        span: Span,
        expression: Expression<'a>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        if is_member_assignment_with_assignment_object(&expression) {
            return self.split_member_assignment_statement(expression);
        }

        match into_sequence_expressions(expression) {
            Ok(expressions) => self.expressions_to_statements(expressions),
            Err(expression) => self.single(self.ast.statement_expression(span, expression)),
        }
    }

    fn split_return_argument(
        &self,
        span: Span,
        argument: Expression<'a>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        match into_sequence_expressions(argument) {
            Ok(expressions) => self.sequence_return_replacements(expressions),
            Err(argument) => self.single(self.ast.statement_return(span, Some(argument))),
        }
    }

    fn split_throw_argument(
        &self,
        span: Span,
        argument: Expression<'a>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        match into_sequence_expressions(argument) {
            Ok(mut expressions) => {
                let last = expressions.pop().expect("sequence has a final expression");
                let mut replacements = self.expressions_to_statements(expressions);
                replacements.push(self.ast.statement_throw(span, last));
                replacements
            }
            Err(argument) => self.single(self.ast.statement_throw(span, argument)),
        }
    }

    fn split_if_statement(
        &self,
        statement: oxc_allocator::Box<'a, oxc_ast::ast::IfStatement<'a>>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let statement = statement.unbox();
        match into_sequence_expressions(statement.test) {
            Ok(mut expressions) => {
                let test = expressions.pop().expect("sequence has a final expression");
                let mut replacements = self.expressions_to_statements(expressions);
                replacements.push(self.ast.statement_if(
                    statement.span,
                    test,
                    statement.consequent,
                    statement.alternate,
                ));
                replacements
            }
            Err(test) => self.single(Statement::IfStatement(self.ast.alloc_if_statement(
                statement.span,
                test,
                statement.consequent,
                statement.alternate,
            ))),
        }
    }

    fn split_switch_statement(
        &self,
        statement: oxc_allocator::Box<'a, oxc_ast::ast::SwitchStatement<'a>>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let statement = statement.unbox();
        let scope_id = statement.scope_id.get();

        match into_sequence_expressions(statement.discriminant) {
            Ok(mut expressions) => {
                let discriminant = expressions.pop().expect("sequence has a final expression");
                let mut replacements = self.expressions_to_statements(expressions);
                replacements.push(self.switch_statement(
                    statement.span,
                    discriminant,
                    statement.cases,
                    scope_id,
                ));
                replacements
            }
            Err(discriminant) => self.single(self.switch_statement(
                statement.span,
                discriminant,
                statement.cases,
                scope_id,
            )),
        }
    }

    fn split_variable_declaration_statement(
        &self,
        declaration: oxc_allocator::Box<'a, VariableDeclaration<'a>>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let declaration = declaration.unbox();
        let mut replacements = self.ast.vec();

        for declarator in declaration.declarations {
            self.push_split_variable_declarator(
                &mut replacements,
                declaration.span,
                declaration.kind,
                declaration.declare,
                declarator,
            );
        }

        replacements
    }

    fn split_for_statement(
        &self,
        statement: oxc_allocator::Box<'a, oxc_ast::ast::ForStatement<'a>>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let mut statement = statement.unbox();
        let mut replacements = self.ast.vec();

        match statement.init.take() {
            Some(ForStatementInit::SequenceExpression(sequence)) => {
                let mut expressions = sequence.unbox().expressions;
                let last = expressions.pop();
                let mut init = None;

                if let Some(last) = last {
                    if matches!(last, Expression::AssignmentExpression(_)) {
                        init = Some(expression_to_for_init(last));
                    } else {
                        expressions.push(last);
                    }
                }

                replacements = self.expressions_to_statements(expressions);
                statement.init = init;
            }
            Some(ForStatementInit::VariableDeclaration(declaration)) => {
                let declaration = declaration.unbox();
                let mut init_declarations = self.ast.vec();

                for mut declarator in declaration.declarations {
                    match declarator.init.take().map(into_sequence_expressions) {
                        Some(Ok(mut expressions)) => {
                            let last = expressions.pop().expect("sequence has a final expression");
                            for expression in expressions {
                                replacements.push(self.expression_statement(expression));
                            }
                            declarator.init = Some(last);
                            init_declarations.push(declarator);
                        }
                        Some(Err(init)) => {
                            declarator.init = Some(init);
                            init_declarations.push(declarator);
                        }
                        None => init_declarations.push(declarator),
                    }
                }

                statement.init = Some(ForStatementInit::VariableDeclaration(
                    self.ast.alloc_variable_declaration(
                        declaration.span,
                        declaration.kind,
                        init_declarations,
                        declaration.declare,
                    ),
                ));
            }
            init => {
                statement.init = init;
            }
        }

        replacements.push(self.for_statement(statement));
        replacements
    }

    fn split_for_in_statement(
        &self,
        statement: oxc_allocator::Box<'a, oxc_ast::ast::ForInStatement<'a>>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let statement = statement.unbox();
        let scope_id = statement.scope_id.get();

        match into_sequence_expressions(statement.right) {
            Ok(mut expressions) => {
                let right = expressions.pop().expect("sequence has a final expression");
                let mut replacements = self.expressions_to_statements(expressions);
                replacements.push(self.for_in_statement(
                    statement.span,
                    statement.left,
                    right,
                    statement.body,
                    scope_id,
                ));
                replacements
            }
            Err(right) => self.single(self.for_in_statement(
                statement.span,
                statement.left,
                right,
                statement.body,
                scope_id,
            )),
        }
    }

    fn split_for_of_statement(
        &self,
        statement: oxc_allocator::Box<'a, oxc_ast::ast::ForOfStatement<'a>>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let statement = statement.unbox();
        let scope_id = statement.scope_id.get();

        match into_sequence_expressions(statement.right) {
            Ok(mut expressions) => {
                let right = expressions.pop().expect("sequence has a final expression");
                let mut replacements = self.expressions_to_statements(expressions);
                replacements.push(self.for_of_statement(
                    statement.span,
                    statement.r#await,
                    statement.left,
                    right,
                    statement.body,
                    scope_id,
                ));
                replacements
            }
            Err(right) => self.single(self.for_of_statement(
                statement.span,
                statement.r#await,
                statement.left,
                right,
                statement.body,
                scope_id,
            )),
        }
    }

    fn push_split_variable_declarator(
        &self,
        replacements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
        declaration_span: Span,
        kind: VariableDeclarationKind,
        declare: bool,
        mut declarator: VariableDeclarator<'a>,
    ) {
        match declarator.init.take().map(into_sequence_expressions) {
            Some(Ok(mut expressions)) => {
                let last = expressions.pop().expect("sequence has a final expression");
                for expression in expressions {
                    replacements.push(self.expression_statement(expression));
                }
                declarator.init = Some(last);
            }
            Some(Err(init)) => {
                declarator.init = Some(init);
            }
            None => {}
        }

        replacements.push(self.variable_declaration_statement(
            declaration_span,
            kind,
            declare,
            declarator,
        ));
    }

    fn sequence_return_replacements(
        &self,
        mut expressions: oxc_allocator::Vec<'a, Expression<'a>>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let last = expressions.pop().expect("sequence has a final expression");
        let mut replacements = self.expressions_to_statements(expressions);

        if let Expression::AssignmentExpression(assignment) = &last {
            if let Some(return_value) =
                assignment_target_to_expression(assignment.left.clone_in(self.ast.allocator))
            {
                let span = assignment.span;
                replacements.push(self.expression_statement(last));
                replacements.push(self.ast.statement_return(span, Some(return_value)));
                return replacements;
            }
        }

        let span = last.span();
        replacements.push(self.ast.statement_return(span, Some(last)));
        replacements
    }

    fn expressions_to_statements(
        &self,
        expressions: oxc_allocator::Vec<'a, Expression<'a>>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let mut statements = self.ast.vec_with_capacity(expressions.len());
        for expression in expressions {
            statements.push(self.expression_statement(expression));
        }
        statements
    }

    fn split_member_assignment_statement(
        &self,
        expression: Expression<'a>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let Expression::AssignmentExpression(assignment) = expression else {
            unreachable!("shape was prechecked");
        };
        let assignment = assignment.unbox();

        let Some((object_assignment, member_target)) =
            self.member_target_without_assignment_object(assignment.left)
        else {
            unreachable!("shape was prechecked");
        };

        let member_assignment = self.ast.expression_assignment(
            assignment.span,
            assignment.operator,
            member_target,
            assignment.right,
        );

        let mut replacements = self.ast.vec_with_capacity(2);
        replacements.push(self.expression_statement(object_assignment));
        replacements.push(self.expression_statement(member_assignment));
        replacements
    }

    fn member_target_without_assignment_object(
        &self,
        target: AssignmentTarget<'a>,
    ) -> Option<(Expression<'a>, AssignmentTarget<'a>)> {
        match target {
            AssignmentTarget::ComputedMemberExpression(member) => {
                let mut member = member.unbox();
                let (object_assignment, object) =
                    self.take_assignment_object_expression(member.object)?;
                member.object = object;
                Some((
                    object_assignment,
                    AssignmentTarget::ComputedMemberExpression(
                        self.ast.alloc_computed_member_expression(
                            member.span,
                            member.object,
                            member.expression,
                            member.optional,
                        ),
                    ),
                ))
            }
            AssignmentTarget::StaticMemberExpression(member) => {
                let mut member = member.unbox();
                let (object_assignment, object) =
                    self.take_assignment_object_expression(member.object)?;
                member.object = object;
                Some((
                    object_assignment,
                    AssignmentTarget::StaticMemberExpression(
                        self.ast.alloc_static_member_expression(
                            member.span,
                            member.object,
                            member.property,
                            member.optional,
                        ),
                    ),
                ))
            }
            _ => None,
        }
    }

    fn take_assignment_object_expression(
        &self,
        expression: Expression<'a>,
    ) -> Option<(Expression<'a>, Expression<'a>)> {
        match expression {
            Expression::ParenthesizedExpression(parenthesized) => {
                self.take_assignment_object_expression(parenthesized.unbox().expression)
            }
            Expression::AssignmentExpression(assignment) => {
                let object = assignment_target_identifier_to_expression(
                    assignment.left.clone_in(self.ast.allocator),
                )?;
                Some((Expression::AssignmentExpression(assignment), object))
            }
            _ => None,
        }
    }

    fn variable_declaration_statement(
        &self,
        span: Span,
        kind: VariableDeclarationKind,
        declare: bool,
        declarator: VariableDeclarator<'a>,
    ) -> Statement<'a> {
        let mut declarations = self.ast.vec_with_capacity(1);
        declarations.push(declarator);

        Statement::VariableDeclaration(self.ast.alloc_variable_declaration(
            span,
            kind,
            declarations,
            declare,
        ))
    }

    fn for_statement(&self, statement: oxc_ast::ast::ForStatement<'a>) -> Statement<'a> {
        match statement.scope_id.get() {
            Some(scope_id) => self.ast.statement_for_with_scope_id(
                statement.span,
                statement.init,
                statement.test,
                statement.update,
                statement.body,
                scope_id,
            ),
            None => self.ast.statement_for(
                statement.span,
                statement.init,
                statement.test,
                statement.update,
                statement.body,
            ),
        }
    }

    fn for_in_statement(
        &self,
        span: Span,
        left: oxc_ast::ast::ForStatementLeft<'a>,
        right: Expression<'a>,
        body: Statement<'a>,
        scope_id: Option<ScopeId>,
    ) -> Statement<'a> {
        match scope_id {
            Some(scope_id) => self
                .ast
                .statement_for_in_with_scope_id(span, left, right, body, scope_id),
            None => self.ast.statement_for_in(span, left, right, body),
        }
    }

    fn for_of_statement(
        &self,
        span: Span,
        r#await: bool,
        left: oxc_ast::ast::ForStatementLeft<'a>,
        right: Expression<'a>,
        body: Statement<'a>,
        scope_id: Option<ScopeId>,
    ) -> Statement<'a> {
        match scope_id {
            Some(scope_id) => self
                .ast
                .statement_for_of_with_scope_id(span, r#await, left, right, body, scope_id),
            None => self.ast.statement_for_of(span, r#await, left, right, body),
        }
    }

    fn switch_statement(
        &self,
        span: Span,
        discriminant: Expression<'a>,
        cases: oxc_allocator::Vec<'a, oxc_ast::ast::SwitchCase<'a>>,
        scope_id: Option<ScopeId>,
    ) -> Statement<'a> {
        match scope_id {
            Some(scope_id) => {
                self.ast
                    .statement_switch_with_scope_id(span, discriminant, cases, scope_id)
            }
            None => self.ast.statement_switch(span, discriminant, cases),
        }
    }

    fn block_statement(&self, body: oxc_allocator::Vec<'a, Statement<'a>>) -> Statement<'a> {
        let span = body
            .first()
            .zip(body.last())
            .map(|(first, last)| Span::new(first.span().start, last.span().end))
            .unwrap_or_default();
        self.ast.statement_block(span, body)
    }

    fn expression_statement(&self, expression: Expression<'a>) -> Statement<'a> {
        let span = expression.span();
        self.ast.statement_expression(span, expression)
    }

    fn single(&self, statement: Statement<'a>) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let mut statements = self.ast.vec_with_capacity(1);
        statements.push(statement);
        statements
    }
}

fn into_sequence_expressions<'a>(
    expression: Expression<'a>,
) -> std::result::Result<oxc_allocator::Vec<'a, Expression<'a>>, Expression<'a>> {
    match expression {
        Expression::SequenceExpression(sequence) => Ok(sequence.unbox().expressions),
        Expression::ParenthesizedExpression(parenthesized) => {
            into_sequence_expressions(parenthesized.unbox().expression)
        }
        expression => Err(expression),
    }
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

fn assignment_target_identifier_to_expression<'a>(
    target: AssignmentTarget<'a>,
) -> Option<Expression<'a>> {
    match target {
        AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
            Some(Expression::Identifier(identifier))
        }
        _ => None,
    }
}

fn is_member_assignment_with_assignment_object(expression: &Expression) -> bool {
    let Expression::AssignmentExpression(assignment) = expression else {
        return false;
    };

    if assignment.operator != AssignmentOperator::Assign {
        return false;
    }

    match &assignment.left {
        AssignmentTarget::ComputedMemberExpression(member) => {
            is_assignment_object_expression(&member.object)
        }
        AssignmentTarget::StaticMemberExpression(member) => {
            is_assignment_object_expression(&member.object)
        }
        _ => false,
    }
}

fn is_assignment_object_expression(expression: &Expression) -> bool {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            is_assignment_object_expression(&parenthesized.expression)
        }
        Expression::AssignmentExpression(assignment) => {
            matches!(
                assignment.left,
                AssignmentTarget::AssignmentTargetIdentifier(_)
            )
        }
        _ => false,
    }
}

fn expression_to_for_init<'a>(expression: Expression<'a>) -> ForStatementInit<'a> {
    macro_rules! init_variant {
        ($variant:ident, $value:ident) => {
            ForStatementInit::$variant($value)
        };
    }

    match expression {
        Expression::BooleanLiteral(value) => init_variant!(BooleanLiteral, value),
        Expression::NullLiteral(value) => init_variant!(NullLiteral, value),
        Expression::NumericLiteral(value) => init_variant!(NumericLiteral, value),
        Expression::BigIntLiteral(value) => init_variant!(BigIntLiteral, value),
        Expression::RegExpLiteral(value) => init_variant!(RegExpLiteral, value),
        Expression::StringLiteral(value) => init_variant!(StringLiteral, value),
        Expression::TemplateLiteral(value) => init_variant!(TemplateLiteral, value),
        Expression::Identifier(value) => init_variant!(Identifier, value),
        Expression::MetaProperty(value) => init_variant!(MetaProperty, value),
        Expression::Super(value) => init_variant!(Super, value),
        Expression::ArrayExpression(value) => init_variant!(ArrayExpression, value),
        Expression::ArrowFunctionExpression(value) => init_variant!(ArrowFunctionExpression, value),
        Expression::AssignmentExpression(value) => init_variant!(AssignmentExpression, value),
        Expression::AwaitExpression(value) => init_variant!(AwaitExpression, value),
        Expression::BinaryExpression(value) => init_variant!(BinaryExpression, value),
        Expression::CallExpression(value) => init_variant!(CallExpression, value),
        Expression::ChainExpression(value) => init_variant!(ChainExpression, value),
        Expression::ClassExpression(value) => init_variant!(ClassExpression, value),
        Expression::ConditionalExpression(value) => init_variant!(ConditionalExpression, value),
        Expression::FunctionExpression(value) => init_variant!(FunctionExpression, value),
        Expression::ImportExpression(value) => init_variant!(ImportExpression, value),
        Expression::LogicalExpression(value) => init_variant!(LogicalExpression, value),
        Expression::NewExpression(value) => init_variant!(NewExpression, value),
        Expression::ObjectExpression(value) => init_variant!(ObjectExpression, value),
        Expression::ParenthesizedExpression(value) => init_variant!(ParenthesizedExpression, value),
        Expression::SequenceExpression(value) => init_variant!(SequenceExpression, value),
        Expression::TaggedTemplateExpression(value) => {
            init_variant!(TaggedTemplateExpression, value)
        }
        Expression::ThisExpression(value) => init_variant!(ThisExpression, value),
        Expression::UnaryExpression(value) => init_variant!(UnaryExpression, value),
        Expression::UpdateExpression(value) => init_variant!(UpdateExpression, value),
        Expression::YieldExpression(value) => init_variant!(YieldExpression, value),
        Expression::PrivateInExpression(value) => init_variant!(PrivateInExpression, value),
        Expression::JSXElement(value) => init_variant!(JSXElement, value),
        Expression::JSXFragment(value) => init_variant!(JSXFragment, value),
        Expression::TSAsExpression(value) => init_variant!(TSAsExpression, value),
        Expression::TSSatisfiesExpression(value) => init_variant!(TSSatisfiesExpression, value),
        Expression::TSTypeAssertion(value) => init_variant!(TSTypeAssertion, value),
        Expression::TSNonNullExpression(value) => init_variant!(TSNonNullExpression, value),
        Expression::TSInstantiationExpression(value) => {
            init_variant!(TSInstantiationExpression, value)
        }
        Expression::ComputedMemberExpression(value) => {
            init_variant!(ComputedMemberExpression, value)
        }
        Expression::StaticMemberExpression(value) => init_variant!(StaticMemberExpression, value),
        Expression::PrivateFieldExpression(value) => init_variant!(PrivateFieldExpression, value),
        Expression::V8IntrinsicExpression(value) => init_variant!(V8IntrinsicExpression, value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn splits_expression_statement_sequence() {
        define_ast_inline_test(transform_ast)(
            r#"
a(), b(), c()
"#,
            r#"
a();
b();
c();
"#,
        );
    }

    #[test]
    fn splits_return_and_arrow_sequence() {
        define_ast_inline_test(transform_ast)(
            r#"
function f() {
  if (a) return b(), c();
  return d = 1, e = 2, f = 3;
}
var foo = (m => (a(), b(), c))();
var bar = (m => (m.a = 1, m.b = 2, m.c = 3))();
"#,
            r#"
function f() {
  if (a) {
    b();
    return c();
  }
  d = 1;
  e = 2;
  f = 3;
  return f;
}
var foo = ((m) => {
  a();
  b();
  return c;
})();
var bar = ((m) => {
  m.a = 1;
  m.b = 2;
  m.c = 3;
  return m.c;
})();
"#,
        );
    }

    #[test]
    fn splits_control_flow_sequence_tests() {
        define_ast_inline_test(transform_ast)(
            r#"
if (a(), b(), c()) {
  d(), e()
}
switch (a(), b(), c()) {
case 1:
  d(), e()
}
"#,
            r#"
a();
b();
if (c()) {
  d();
  e();
}
a();
b();
switch (c()) {
  case 1:
    d();
    e();
}
"#,
        );
    }

    #[test]
    fn splits_variable_and_for_sequences() {
        define_ast_inline_test(transform_ast)(
            r#"
const x = (a(), b(), c()), y = 3, z = (d(), e());
for (a(), b(); c(); d(), e()) {
  f(), g()
}
for (let x = (a(), b(), c()), y = 1; x < 10; x++) {
  d(), e()
}
for (let x in (a(), b(), c())) {
  console.log(x);
}
"#,
            r#"
a();
b();
const x = c();
const y = 3;
d();
const z = e();
a();
b();
for (; c(); d(), e()) {
  f();
  g();
}
a();
b();
for (let x = c(), y = 1; x < 10; x++) {
  d();
  e();
}
a();
b();
for (let x in c()) {
  console.log(x);
}
"#,
        );
    }

    #[test]
    fn splits_member_expression_in_assignment() {
        define_ast_inline_test(transform_ast)(
            r#"
(a = b())['c'] = d;
(a = v).b = c;
"#,
            r#"
a = b();
a["c"] = d;
a = v;
a.b = c;
"#,
        );
    }
}
