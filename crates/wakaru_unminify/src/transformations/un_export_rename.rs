use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{
        BindingIdentifier, BindingPattern, Class, Declaration, Expression, Function,
        IdentifierReference, ImportOrExportKind, ModuleExportName, Program, Statement,
        VariableDeclaration, VariableDeclarator, WithClause,
    },
    AstBuilder,
};
use oxc_ast_visit::VisitMut;
use oxc_semantic::{Scoping, SemanticBuilder, SymbolId};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();

    let mut transformer = ExportRenameInliner {
        ast: AstBuilder::new(source.allocator),
        scoping: &scoping,
    };

    transformer.transform_program(&mut source.program);

    Ok(())
}

struct ExportRenameInliner<'a, 's> {
    ast: AstBuilder<'a>,
    scoping: &'s Scoping,
}

impl<'a> ExportRenameInliner<'a, '_> {
    fn transform_program(&mut self, program: &mut Program<'a>) {
        while let Some(operation) = find_operation(&program.body, self.scoping) {
            self.apply_operation(program, operation);
        }
    }

    fn apply_operation(&self, program: &mut Program<'a>, operation: ExportRenameOperation) {
        let old_body = program.body.take_in(self.ast);
        let mut new_body = self.ast.vec_with_capacity(old_body.len());

        for (index, statement) in old_body.into_iter().enumerate() {
            if index == operation.source_index {
                self.push_inlined_source(statement, operation.target_symbol, &mut new_body);
            } else if index == operation.export_index {
                self.push_alias_export_remainder(statement, &operation, &mut new_body);
            } else {
                new_body.push(statement);
            }
        }

        program.body = new_body;

        let mut renamer = SymbolRenamer {
            ast: self.ast,
            scoping: self.scoping,
            target_symbol: operation.target_symbol,
            new_name: operation.new_name,
        };
        renamer.visit_program(program);
    }

    fn push_inlined_source(
        &self,
        statement: Statement<'a>,
        target_symbol: SymbolId,
        statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        match statement {
            Statement::VariableDeclaration(declaration) => {
                self.push_inlined_variable_declaration(declaration, target_symbol, statements);
            }
            Statement::FunctionDeclaration(function) => {
                statements.push(self.export_statement(Declaration::FunctionDeclaration(function)));
            }
            Statement::ClassDeclaration(class) => {
                statements.push(self.export_statement(Declaration::ClassDeclaration(class)));
            }
            statement => statements.push(statement),
        }
    }

    fn push_inlined_variable_declaration(
        &self,
        mut declaration: oxc_allocator::Box<'a, VariableDeclaration<'a>>,
        target_symbol: SymbolId,
        statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        let span = declaration.span;
        let kind = declaration.kind;
        let declare = declaration.declare;
        let old_declarators = declaration.declarations.take_in(self.ast);
        let mut kept_declarators = self.ast.vec();
        let mut exported_declarator = None;

        for declarator in old_declarators {
            if binding_symbol_id(&declarator.id) == Some(target_symbol) {
                exported_declarator = Some(declarator);
            } else {
                kept_declarators.push(declarator);
            }
        }

        if !kept_declarators.is_empty() {
            statements.push(Statement::VariableDeclaration(
                self.ast
                    .alloc_variable_declaration(span, kind, kept_declarators, declare),
            ));
        }

        if let Some(declarator) = exported_declarator {
            let mut declarations = self.ast.vec_with_capacity(1);
            declarations.push(declarator);
            statements.push(self.export_statement(
                Declaration::VariableDeclaration(self.ast.alloc_variable_declaration(
                    span,
                    kind,
                    declarations,
                    declare,
                )),
            ));
        }
    }

    fn push_alias_export_remainder(
        &self,
        statement: Statement<'a>,
        operation: &ExportRenameOperation,
        statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        let Statement::ExportNamedDeclaration(mut export_declaration) = statement else {
            statements.push(statement);
            return;
        };

        match &mut export_declaration.declaration {
            Some(Declaration::VariableDeclaration(variable_declaration)) => {
                let old_declarations = variable_declaration.declarations.take_in(self.ast);
                let mut kept_declarations = self.ast.vec();

                for declarator in old_declarations {
                    if binding_symbol_id(&declarator.id) != operation.alias_symbol {
                        kept_declarations.push(declarator);
                    }
                }

                if kept_declarations.is_empty() {
                    return;
                }

                variable_declaration.declarations = kept_declarations;
                statements.push(Statement::ExportNamedDeclaration(export_declaration));
            }
            None => {
                let old_specifiers = export_declaration.specifiers.take_in(self.ast);
                let mut kept_specifiers = self.ast.vec();

                for specifier in old_specifiers {
                    if export_local_symbol_id(&specifier.local, self.scoping)
                        != Some(operation.target_symbol)
                    {
                        kept_specifiers.push(specifier);
                    }
                }

                if kept_specifiers.is_empty() {
                    return;
                }

                export_declaration.specifiers = kept_specifiers;
                statements.push(Statement::ExportNamedDeclaration(export_declaration));
            }
            _ => statements.push(Statement::ExportNamedDeclaration(export_declaration)),
        }
    }

    fn export_statement(&self, declaration: Declaration<'a>) -> Statement<'a> {
        Statement::ExportNamedDeclaration(self.ast.alloc_export_named_declaration(
            declaration.span(),
            Some(declaration),
            self.ast.vec(),
            None,
            ImportOrExportKind::Value,
            None::<oxc_allocator::Box<'a, WithClause<'a>>>,
        ))
    }
}

struct SymbolRenamer<'a, 's> {
    ast: AstBuilder<'a>,
    scoping: &'s Scoping,
    target_symbol: SymbolId,
    new_name: String,
}

impl<'a> VisitMut<'a> for SymbolRenamer<'a, '_> {
    fn visit_binding_identifier(&mut self, identifier: &mut BindingIdentifier<'a>) {
        if identifier.symbol_id.get() == Some(self.target_symbol) {
            identifier.name = self.ast.ident(&self.new_name);
        }
    }

    fn visit_identifier_reference(&mut self, identifier: &mut IdentifierReference<'a>) {
        if identifier
            .reference_id
            .get()
            .and_then(|reference_id| self.scoping.get_reference(reference_id).symbol_id())
            == Some(self.target_symbol)
        {
            identifier.name = self.ast.ident(&self.new_name);
        }
    }
}

struct ExportRenameOperation {
    export_index: usize,
    source_index: usize,
    target_symbol: SymbolId,
    alias_symbol: Option<SymbolId>,
    new_name: String,
}

fn find_operation(
    body: &oxc_allocator::Vec<Statement<'_>>,
    scoping: &Scoping,
) -> Option<ExportRenameOperation> {
    for (export_index, statement) in body.iter().enumerate() {
        let Some(candidate) = export_alias_candidate(statement, scoping) else {
            continue;
        };

        if has_conflicting_root_binding(scoping, &candidate.new_name, candidate.alias_symbol) {
            continue;
        }

        let Some(source_index) = find_source_index(body, candidate.target_symbol) else {
            continue;
        };

        return Some(ExportRenameOperation {
            export_index,
            source_index,
            target_symbol: candidate.target_symbol,
            alias_symbol: candidate.alias_symbol,
            new_name: candidate.new_name,
        });
    }

    None
}

struct ExportAliasCandidate {
    target_symbol: SymbolId,
    alias_symbol: Option<SymbolId>,
    new_name: String,
}

fn export_alias_candidate(
    statement: &Statement<'_>,
    scoping: &Scoping,
) -> Option<ExportAliasCandidate> {
    let Statement::ExportNamedDeclaration(export_declaration) = statement else {
        return None;
    };

    if export_declaration.source.is_some() {
        return None;
    }

    if let Some(Declaration::VariableDeclaration(variable_declaration)) =
        &export_declaration.declaration
    {
        return variable_declaration
            .declarations
            .iter()
            .find_map(|declarator| export_variable_alias_candidate(declarator, scoping));
    }

    if export_declaration.declaration.is_none() {
        return export_declaration
            .specifiers
            .iter()
            .find_map(|specifier| export_specifier_alias_candidate(specifier, scoping));
    }

    None
}

fn export_variable_alias_candidate(
    declarator: &VariableDeclarator<'_>,
    scoping: &Scoping,
) -> Option<ExportAliasCandidate> {
    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return None;
    };

    let Some(Expression::Identifier(init)) = &declarator.init else {
        return None;
    };

    Some(ExportAliasCandidate {
        target_symbol: reference_symbol_id(init, scoping)?,
        alias_symbol: identifier.symbol_id.get(),
        new_name: identifier.name.as_str().to_string(),
    })
}

fn export_specifier_alias_candidate(
    specifier: &oxc_ast::ast::ExportSpecifier<'_>,
    scoping: &Scoping,
) -> Option<ExportAliasCandidate> {
    let target_symbol = export_local_symbol_id(&specifier.local, scoping)?;
    let new_name = module_export_name(&specifier.exported)?.to_string();

    Some(ExportAliasCandidate {
        target_symbol,
        alias_symbol: None,
        new_name,
    })
}

fn find_source_index(
    body: &oxc_allocator::Vec<Statement<'_>>,
    target_symbol: SymbolId,
) -> Option<usize> {
    let mut source_index = None;

    for (index, statement) in body.iter().enumerate() {
        if top_level_statement_has_binding_symbol(statement, target_symbol) {
            if source_index.is_some() {
                return None;
            }
            source_index = Some(index);
        }
    }

    source_index
}

fn top_level_statement_has_binding_symbol(
    statement: &Statement<'_>,
    target_symbol: SymbolId,
) -> bool {
    match statement {
        Statement::VariableDeclaration(declaration) => declaration
            .declarations
            .iter()
            .any(|declarator| binding_symbol_id(&declarator.id) == Some(target_symbol)),
        Statement::FunctionDeclaration(function) => {
            function_binding_symbol_id(function) == Some(target_symbol)
        }
        Statement::ClassDeclaration(class) => class_binding_symbol_id(class) == Some(target_symbol),
        _ => false,
    }
}

fn has_conflicting_root_binding(
    scoping: &Scoping,
    new_name: &str,
    allowed_symbol: Option<SymbolId>,
) -> bool {
    let Some(symbol_id) = scoping.get_root_binding(new_name.into()) else {
        return false;
    };

    Some(symbol_id) != allowed_symbol
}

fn binding_symbol_id(binding: &BindingPattern<'_>) -> Option<SymbolId> {
    let BindingPattern::BindingIdentifier(identifier) = binding else {
        return None;
    };

    identifier.symbol_id.get()
}

fn function_binding_symbol_id(function: &Function<'_>) -> Option<SymbolId> {
    function
        .id
        .as_ref()
        .and_then(|identifier| identifier.symbol_id.get())
}

fn class_binding_symbol_id(class: &Class<'_>) -> Option<SymbolId> {
    class
        .id
        .as_ref()
        .and_then(|identifier| identifier.symbol_id.get())
}

fn reference_symbol_id(
    identifier: &IdentifierReference<'_>,
    scoping: &Scoping,
) -> Option<SymbolId> {
    identifier
        .reference_id
        .get()
        .and_then(|reference_id| scoping.get_reference(reference_id).symbol_id())
}

fn export_local_symbol_id(local: &ModuleExportName<'_>, scoping: &Scoping) -> Option<SymbolId> {
    match local {
        ModuleExportName::IdentifierReference(identifier) => {
            reference_symbol_id(identifier, scoping)
        }
        ModuleExportName::IdentifierName(identifier) => {
            scoping.get_root_binding(identifier.name.as_str().into())
        }
        ModuleExportName::StringLiteral(_) => None,
    }
}

fn module_export_name<'a>(name: &'a ModuleExportName<'a>) -> Option<&'a str> {
    match name {
        ModuleExportName::IdentifierName(identifier) => Some(identifier.name.as_str()),
        ModuleExportName::IdentifierReference(identifier) => Some(identifier.name.as_str()),
        ModuleExportName::StringLiteral(_) => None,
    }
}

trait DeclarationSpan {
    fn span(&self) -> oxc_span::Span;
}

impl DeclarationSpan for Declaration<'_> {
    fn span(&self) -> oxc_span::Span {
        match self {
            Declaration::VariableDeclaration(declaration) => declaration.span,
            Declaration::FunctionDeclaration(function) => function.span,
            Declaration::ClassDeclaration(class) => class.span,
            Declaration::TSTypeAliasDeclaration(declaration) => declaration.span,
            Declaration::TSInterfaceDeclaration(declaration) => declaration.span,
            Declaration::TSEnumDeclaration(declaration) => declaration.span,
            Declaration::TSModuleDeclaration(declaration) => declaration.span,
            Declaration::TSGlobalDeclaration(declaration) => declaration.span,
            Declaration::TSImportEqualsDeclaration(declaration) => declaration.span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn merges_variable_declaration_and_export_alias() {
        define_ast_inline_test(transform_ast)(
            "
const a = 1;
console.log(a);
export const b = a, c = 2;
",
            "
export const b = 1;
console.log(b);
export const c = 2;
",
        );
    }

    #[test]
    fn merges_function_declaration_and_export_alias() {
        define_ast_inline_test(transform_ast)(
            "
function a() {}
export const b = a;
",
            "
export function b() {}
",
        );
    }

    #[test]
    fn renames_recursive_function_references_without_touching_shadows() {
        define_ast_inline_test(transform_ast)(
            "
function test() {
    function a() {}
}
function a(n) {
    if (n < 2) return n;
    return a(n - 1) + a(n - 2);
}
export const fib = a;
",
            "
function test() {
  function a() {}
}
export function fib(n) {
  if (n < 2) return n;
  return fib(n - 1) + fib(n - 2);
}
",
        );
    }

    #[test]
    fn merges_class_declaration_and_export_alias() {
        define_ast_inline_test(transform_ast)(
            "
class o {}
export const App = o;
",
            "
export class App {}
",
        );
    }

    #[test]
    fn preserves_new_name_conflicts() {
        define_ast_inline_test(transform_ast)(
            "
const a = 1;
const b = 2;
export { b as a };
",
            "
const a = 1;
const b = 2;
export { b as a };
",
        );
    }

    #[test]
    fn does_not_modify_export_default() {
        define_ast_inline_test(transform_ast)(
            "
const o = class {};
export default o;
",
            "
const o = class {};
export default o;
",
        );
    }

    #[test]
    fn renames_only_references_to_the_root_binding() {
        define_ast_inline_test(transform_ast)(
            "
const a = 1;
console.log(a);
{
    const a = 2;
    console.log(a);
}
function test() {
    const a = 3;
    console.log(a);
}
for (let a = 4; a < 5; a++) {
    console.log(a);
}
export const b = a, c = 2;
",
            "
export const b = 1;
console.log(b);
{
  const a = 2;
  console.log(a);
}
function test() {
  const a = 3;
  console.log(a);
}
for (let a = 4; a < 5; a++) {
  console.log(a);
}
export const c = 2;
",
        );
    }
}
