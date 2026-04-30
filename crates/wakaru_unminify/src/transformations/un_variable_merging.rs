use std::collections::HashSet;

use oxc_allocator::{CloneIn, TakeIn};
use oxc_ast::{
    ast::{
        BindingPattern, Declaration, ForStatement, ForStatementInit, IdentifierReference,
        Statement, VariableDeclaration, VariableDeclarationKind, VariableDeclarator,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk, walk_mut, Visit, VisitMut};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut splitter = VariableMergingSplitter {
        ast: AstBuilder::new(source.allocator),
    };

    splitter.visit_program(&mut source.program);

    Ok(())
}

struct VariableMergingSplitter<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for VariableMergingSplitter<'a> {
    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        walk_mut::walk_statements(self, statements);

        let outer_declared_names = collect_direct_declared_names(statements);
        let old_statements = statements.take_in(self.ast);
        let mut new_statements = self.ast.vec_with_capacity(old_statements.len());

        for statement in old_statements {
            let replacements = self.split_statement(statement, &outer_declared_names);
            for replacement in replacements {
                new_statements.push(replacement);
            }
        }

        *statements = new_statements;
    }
}

impl<'a> VariableMergingSplitter<'a> {
    fn split_statement(
        &self,
        statement: Statement<'a>,
        outer_declared_names: &HashSet<String>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        match statement {
            Statement::ForStatement(for_statement) => {
                self.split_for_statement(for_statement, outer_declared_names)
            }
            Statement::VariableDeclaration(declaration) => {
                self.split_variable_declaration_statement(declaration)
            }
            Statement::ExportNamedDeclaration(export_declaration) => {
                self.split_export_variable_declaration_statement(export_declaration)
            }
            statement => {
                let mut statements = self.ast.vec_with_capacity(1);
                statements.push(statement);
                statements
            }
        }
    }

    fn split_variable_declaration_statement(
        &self,
        mut declaration: oxc_allocator::Box<'a, VariableDeclaration<'a>>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        if declaration.declarations.len() <= 1 {
            let mut statements = self.ast.vec_with_capacity(1);
            statements.push(Statement::VariableDeclaration(declaration));
            return statements;
        }

        let span = declaration.span;
        let kind = declaration.kind;
        let declare = declaration.declare;
        let old_declarations = declaration.declarations.take_in(self.ast);
        let mut replacements = self.ast.vec_with_capacity(old_declarations.len());

        for declarator in old_declarations {
            replacements.push(self.variable_declaration_statement(span, kind, declare, declarator));
        }

        replacements
    }

    fn split_export_variable_declaration_statement(
        &self,
        export_declaration: oxc_allocator::Box<'a, oxc_ast::ast::ExportNamedDeclaration<'a>>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let Some(Declaration::VariableDeclaration(mut variable_declaration)) =
            export_declaration.declaration.clone_in(self.ast.allocator)
        else {
            let mut statements = self.ast.vec_with_capacity(1);
            statements.push(Statement::ExportNamedDeclaration(export_declaration));
            return statements;
        };

        if variable_declaration.declarations.len() <= 1 {
            let mut statements = self.ast.vec_with_capacity(1);
            statements.push(Statement::ExportNamedDeclaration(export_declaration));
            return statements;
        }

        let span = variable_declaration.span;
        let kind = variable_declaration.kind;
        let declare = variable_declaration.declare;
        let old_declarations = variable_declaration.declarations.take_in(self.ast);
        let mut replacements = self.ast.vec_with_capacity(old_declarations.len());

        for declarator in old_declarations {
            let declaration = self.variable_declaration(span, kind, declare, declarator);
            let export = self.ast.alloc_export_named_declaration(
                export_declaration.span,
                Some(Declaration::VariableDeclaration(declaration)),
                export_declaration.specifiers.clone_in(self.ast.allocator),
                export_declaration.source.clone_in(self.ast.allocator),
                export_declaration.export_kind,
                export_declaration.with_clause.clone_in(self.ast.allocator),
            );

            replacements.push(Statement::ExportNamedDeclaration(export));
        }

        replacements
    }

    fn split_for_statement(
        &self,
        mut for_statement: oxc_allocator::Box<'a, ForStatement<'a>>,
        outer_declared_names: &HashSet<String>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let Some(ForStatementInit::VariableDeclaration(init)) = &for_statement.init else {
            let mut statements = self.ast.vec_with_capacity(1);
            statements.push(Statement::ForStatement(for_statement));
            return statements;
        };

        if init.kind != VariableDeclarationKind::Var {
            let mut statements = self.ast.vec_with_capacity(1);
            statements.push(Statement::ForStatement(for_statement));
            return statements;
        }

        let keep_flags = self.for_init_keep_flags(&for_statement, outer_declared_names);
        if keep_flags.iter().all(|keep| *keep) {
            let mut statements = self.ast.vec_with_capacity(1);
            statements.push(Statement::ForStatement(for_statement));
            return statements;
        }

        let mut replacements = self.ast.vec_with_capacity(keep_flags.len() + 1);
        let mut kept_declarations = self.ast.vec();

        let (span, kind, declare, old_declarations) = {
            let Some(ForStatementInit::VariableDeclaration(init)) = for_statement.init.as_mut()
            else {
                let mut statements = self.ast.vec_with_capacity(1);
                statements.push(Statement::ForStatement(for_statement));
                return statements;
            };

            (
                init.span,
                init.kind,
                init.declare,
                init.declarations.take_in(self.ast),
            )
        };

        for (declarator, keep) in old_declarations.into_iter().zip(keep_flags) {
            if keep {
                kept_declarations.push(declarator);
            } else {
                replacements
                    .push(self.variable_declaration_statement(span, kind, declare, declarator));
            }
        }

        if kept_declarations.is_empty() {
            for_statement.init = None;
        } else if let Some(ForStatementInit::VariableDeclaration(init)) =
            for_statement.init.as_mut()
        {
            init.declarations = kept_declarations;
        }

        replacements.push(Statement::ForStatement(for_statement));

        replacements
    }

    fn for_init_keep_flags(
        &self,
        for_statement: &ForStatement<'a>,
        outer_declared_names: &HashSet<String>,
    ) -> std::vec::Vec<bool> {
        let Some(ForStatementInit::VariableDeclaration(init)) = &for_statement.init else {
            return std::vec::Vec::new();
        };

        init.declarations
            .iter()
            .map(|declarator| {
                let Some(name) = binding_identifier_name(declarator) else {
                    return false;
                };

                outer_declared_names.contains(name) || has_identifier_reference(for_statement, name)
            })
            .collect()
    }

    fn variable_declaration_statement(
        &self,
        span: oxc_span::Span,
        kind: VariableDeclarationKind,
        declare: bool,
        declarator: VariableDeclarator<'a>,
    ) -> Statement<'a> {
        Statement::VariableDeclaration(self.variable_declaration(span, kind, declare, declarator))
    }

    fn variable_declaration(
        &self,
        span: oxc_span::Span,
        kind: VariableDeclarationKind,
        declare: bool,
        declarator: VariableDeclarator<'a>,
    ) -> oxc_allocator::Box<'a, VariableDeclaration<'a>> {
        let mut declarations = self.ast.vec_with_capacity(1);
        declarations.push(declarator);
        self.ast
            .alloc_variable_declaration(span, kind, declarations, declare)
    }
}

fn collect_direct_declared_names<'a>(
    statements: &oxc_allocator::Vec<'a, Statement<'a>>,
) -> HashSet<String> {
    let mut names = HashSet::new();

    for statement in statements {
        match statement {
            Statement::VariableDeclaration(declaration) => {
                collect_declaration_names(&declaration.declarations, &mut names);
            }
            Statement::ExportNamedDeclaration(export_declaration) => {
                if let Some(Declaration::VariableDeclaration(declaration)) =
                    &export_declaration.declaration
                {
                    collect_declaration_names(&declaration.declarations, &mut names);
                }
            }
            _ => {}
        }
    }

    names
}

fn collect_declaration_names<'a>(
    declarations: &oxc_allocator::Vec<'a, VariableDeclarator<'a>>,
    names: &mut HashSet<String>,
) {
    for declarator in declarations {
        if let Some(name) = binding_identifier_name(declarator) {
            names.insert(name.to_string());
        }
    }
}

fn binding_identifier_name<'a>(declarator: &'a VariableDeclarator<'a>) -> Option<&'a str> {
    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return None;
    };

    Some(identifier.name.as_str())
}

fn has_identifier_reference(for_statement: &ForStatement, name: &str) -> bool {
    let mut finder = IdentifierReferenceFinder { name, found: false };
    finder.visit_for_statement(for_statement);
    finder.found
}

struct IdentifierReferenceFinder<'a> {
    name: &'a str,
    found: bool,
}

impl<'a> Visit<'a> for IdentifierReferenceFinder<'_> {
    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        if identifier.name.as_str() == self.name {
            self.found = true;
        }
    }

    fn visit_for_statement(&mut self, for_statement: &ForStatement<'a>) {
        if !self.found {
            walk::walk_for_statement(self, for_statement);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn splits_variable_declarations() {
        define_ast_inline_test(transform_ast)(
            "
var a= 1, b = true, c = \"hello\", d = 1.2, e = [1, 2, 3], f = {a: 1, b: 2, c: 3}, g = function() { return 1; }, h = () => 1,
{ i: j } = k, [l, m] = n;
",
            "
var a = 1;
var b = true;
var c = \"hello\";
var d = 1.2;
var e = [
  1,
  2,
  3
];
var f = {
  a: 1,
  b: 2,
  c: 3
};
var g = function() {
  return 1;
};
var h = () => 1;
var { i: j } = k;
var [l, m] = n;
",
        );
    }

    #[test]
    fn preserves_declaration_kind() {
        define_ast_inline_test(transform_ast)(
            "
var a = 1, b = 2, c = 3;
let d = 1, e = 2, f = 3;
const g = 1, h = 2, i = 3;
",
            "
var a = 1;
var b = 2;
var c = 3;
let d = 1;
let e = 2;
let f = 3;
const g = 1;
const h = 2;
const i = 3;
",
        );
    }

    #[test]
    fn splits_export_variable_declarations() {
        define_ast_inline_test(transform_ast)(
            "
export var a= 1, b = true, c = \"hello\";
",
            "
export var a = 1;
export var b = true;
export var c = \"hello\";
",
        );
    }

    #[test]
    fn extracts_unused_var_declarators_from_for_init() {
        define_ast_inline_test(transform_ast)(
            "
for (var i = 0, j = 0, k = 0; j < 10; k++) {
  console.log(k);
}

for (var _len = arguments.length, _arguments = new Array(_len > 2 ? _len - 2 : 0), _key = 2; _key < _len; _key++) {
  _arguments[_key - 2] = arguments[_key];
}
",
            "
var i = 0;
for (var j = 0, k = 0; j < 10; k++) {
  console.log(k);
}
for (var _len = arguments.length, _arguments = new Array(_len > 2 ? _len - 2 : 0), _key = 2; _key < _len; _key++) {
  _arguments[_key - 2] = arguments[_key];
}
",
        );
    }

    #[test]
    fn leaves_non_var_for_declarations_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
for (let i = 0, j = 0, k = 0; j < 10; k++) {}
for (const i = 0, j = 0, k = 0; j < 10; k++) {}
",
            "
for (let i = 0, j = 0, k = 0; j < 10; k++) {}
for (const i = 0, j = 0, k = 0; j < 10; k++) {}
",
        );
    }

    #[test]
    fn prunes_empty_for_init() {
        define_ast_inline_test(transform_ast)(
            "
for (var i = 0; j < 10; k++) {}
",
            "
var i = 0;
for (; j < 10; k++) {}
",
        );
    }

    #[test]
    fn keeps_for_declarator_declared_in_parent_statement_list() {
        define_ast_inline_test(transform_ast)(
            "
var i = 99;
for (var i = 0, j = 0, k = 0; j < 10; j++) {}
",
            "
var i = 99;
var k = 0;
for (var i = 0, j = 0; j < 10; j++) {}
",
        );
    }
}
