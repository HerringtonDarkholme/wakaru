use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{ArrowFunctionExpression, FunctionBody, Statement, SwitchCase, VariableDeclarationKind},
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::GetSpan;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut blockifier = CurlyBraceBlockifier {
        ast: AstBuilder::new(source.allocator),
    };

    blockifier.visit_program(&mut source.program);

    Ok(())
}

struct CurlyBraceBlockifier<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for CurlyBraceBlockifier<'a> {
    fn visit_statement(&mut self, statement: &mut Statement<'a>) {
        walk_mut::walk_statement(self, statement);

        match statement {
            Statement::IfStatement(if_statement) => {
                self.blockify_statement(&mut if_statement.consequent);

                if let Some(alternate) = &mut if_statement.alternate {
                    if !matches!(alternate, Statement::IfStatement(_)) {
                        self.blockify_statement(alternate);
                    }
                }
            }
            Statement::ForStatement(for_statement) => {
                self.blockify_statement(&mut for_statement.body);
            }
            Statement::ForInStatement(for_in_statement) => {
                self.blockify_statement(&mut for_in_statement.body);
            }
            Statement::ForOfStatement(for_of_statement) => {
                self.blockify_statement(&mut for_of_statement.body);
            }
            Statement::WhileStatement(while_statement) => {
                self.blockify_statement(&mut while_statement.body);
            }
            Statement::DoWhileStatement(do_while_statement) => {
                self.blockify_statement(&mut do_while_statement.body);
            }
            _ => {}
        }
    }

    fn visit_arrow_function_expression(&mut self, arrow: &mut ArrowFunctionExpression<'a>) {
        walk_mut::walk_arrow_function_expression(self, arrow);

        if !arrow.expression {
            return;
        }

        let Some(return_statement) = self.arrow_expression_return_statement(arrow.body.as_mut())
        else {
            return;
        };

        arrow.expression = false;
        arrow.body.statements = self.ast.vec1(return_statement);
    }

    fn visit_switch_case(&mut self, switch_case: &mut SwitchCase<'a>) {
        walk_mut::walk_switch_case(self, switch_case);

        if switch_case.consequent.is_empty()
            || matches!(
                switch_case.consequent.first(),
                Some(Statement::BlockStatement(_))
            )
        {
            return;
        }

        let span = switch_case.span;
        let old_consequent = switch_case.consequent.take_in(self.ast);
        let block = self.ast.statement_block(span, old_consequent);
        switch_case.consequent = self.ast.vec1(block);
    }
}

impl<'a> CurlyBraceBlockifier<'a> {
    fn blockify_statement(&self, statement: &mut Statement<'a>) {
        if !should_blockify_statement(statement) {
            return;
        }

        let span = statement.span();
        let old_statement = statement.take_in(self.ast);
        let body = if matches!(old_statement, Statement::EmptyStatement(_)) {
            self.ast.vec()
        } else {
            self.ast.vec1(old_statement)
        };
        *statement = self.ast.statement_block(span, body);
    }

    fn arrow_expression_return_statement(
        &self,
        body: &mut FunctionBody<'a>,
    ) -> Option<Statement<'a>> {
        if body.statements.len() != 1 {
            return None;
        }

        let mut statements = body.statements.take_in(self.ast);
        let Statement::ExpressionStatement(mut expression_statement) = statements.pop()? else {
            body.statements = statements;
            return None;
        };

        let expression = expression_statement.expression.take_in(self.ast);
        Some(
            self.ast
                .statement_return(expression_statement.span, Some(expression)),
        )
    }
}

fn should_blockify_statement(statement: &Statement) -> bool {
    !matches!(statement, Statement::BlockStatement(_)) && !is_var_declaration(statement)
}

fn is_var_declaration(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::VariableDeclaration(declaration)
            if declaration.kind == VariableDeclarationKind::Var
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn blockifies_control_flow_and_arrow_expressions() {
        define_ast_inline_test(transform_ast)(
            "
if (a) b();

if (a) b();
else if (c) d();
else e();

for (let i = 0; i < 10; i++) b();

for (let i in a) b();

for (let i of a) b();

while (a) b();

while (a);

do
  b();
while (a);

() => b();

label: b();
",
            "
if (a) {
  b();
}
if (a) {
  b();
} else if (c) {
  d();
} else {
  e();
}
for (let i = 0; i < 10; i++) {
  b();
}
for (let i in a) {
  b();
}
for (let i of a) {
  b();
}
while (a) {
  b();
}
while (a) {}
do {
  b();
} while (a);
() => {
  return b();
};
label: b();
",
        );
    }

    #[test]
    fn does_not_blockify_direct_var_declarations() {
        define_ast_inline_test(transform_ast)(
            "
if (a) var b = 1;
else if (c) var d = 1;
else var e = 1;

for (let i = 0; i < 10; i++) var f = 1;

for (let i in a) var g = 1;

for (let i of a) var h = 1;

while (a) var i = 1;

do
  var j = 1;
while (a);
",
            "
if (a) var b = 1;
else if (c) var d = 1;
else var e = 1;
for (let i = 0; i < 10; i++) var f = 1;
for (let i in a) var g = 1;
for (let i of a) var h = 1;
while (a) var i = 1;
do
  var j = 1;
while (a);
",
        );
    }

    #[test]
    fn blockifies_switch_case_consequents() {
        define_ast_inline_test(transform_ast)(
            "
switch (a) {
  case 1:
    b();
    break;
  case 2:
    {
      c();
    }
}
",
            "
switch (a) {
  case 1: {
    b();
    break;
  }
  case 2: {
    c();
  }
}
",
        );
    }
}
