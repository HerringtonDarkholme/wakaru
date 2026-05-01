use std::collections::{HashMap, HashSet};

use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{
        CallExpression, Expression, ImportDeclaration, ImportDeclarationSpecifier,
        ImportOrExportKind,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_semantic::{Scoping, SemanticBuilder, SymbolId};
use oxc_span::Span;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();

    let mut imports = ImportState::from_program(&source.program, &scoping);
    let mut replacer = IndirectCallReplacer {
        ast: AstBuilder::new(source.allocator),
        scoping: &scoping,
        imports: &mut imports,
    };
    replacer.visit_program(&mut source.program);

    let mut import_rewriter = ImportRewriter {
        ast: AstBuilder::new(source.allocator),
        imports,
        scoping: &scoping,
    };
    import_rewriter.transform_program(&mut source.program);

    Ok(())
}

#[derive(Default)]
struct ImportState {
    default_imports: HashMap<SymbolId, DefaultImport>,
    named_imports: HashMap<(String, String), String>,
    additions: HashMap<String, Vec<NamedImportAddition>>,
    addition_keys: HashSet<(String, String, String)>,
    occupied_names: HashSet<String>,
    replaced_default_refs: HashMap<SymbolId, usize>,
    replacement_cache: HashMap<(SymbolId, String), String>,
}

struct DefaultImport {
    source: String,
}

struct NamedImportAddition {
    imported: String,
    local: String,
}

impl ImportState {
    fn from_program(program: &oxc_ast::ast::Program, scoping: &Scoping) -> Self {
        let mut state = Self {
            occupied_names: collect_all_names(scoping),
            ..Self::default()
        };

        for statement in &program.body {
            let oxc_ast::ast::Statement::ImportDeclaration(import) = statement else {
                continue;
            };

            state.collect_import(import);
        }

        state
    }

    fn collect_import(&mut self, import: &ImportDeclaration) {
        let source = import.source.value.as_str().to_string();
        let Some(specifiers) = &import.specifiers else {
            return;
        };

        for specifier in specifiers {
            match specifier {
                ImportDeclarationSpecifier::ImportDefaultSpecifier(default) => {
                    if let Some(symbol_id) = default.local.symbol_id.get() {
                        self.default_imports.insert(
                            symbol_id,
                            DefaultImport {
                                source: source.clone(),
                            },
                        );
                    }
                    self.occupied_names
                        .insert(default.local.name.as_str().to_string());
                }
                ImportDeclarationSpecifier::ImportSpecifier(named) => {
                    let Some(imported) = imported_name(&named.imported) else {
                        continue;
                    };
                    self.named_imports.insert(
                        (source.clone(), imported.to_string()),
                        named.local.name.as_str().to_string(),
                    );
                    self.occupied_names
                        .insert(named.local.name.as_str().to_string());
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace) => {
                    self.occupied_names
                        .insert(namespace.local.name.as_str().to_string());
                }
            }
        }
    }

    fn local_for_named_import(
        &mut self,
        symbol_id: SymbolId,
        source: &str,
        imported: &str,
    ) -> String {
        let cache_key = (symbol_id, imported.to_string());
        if let Some(local) = self.replacement_cache.get(&cache_key) {
            return local.clone();
        }

        if let Some(local) = self
            .named_imports
            .get(&(source.to_string(), imported.to_string()))
        {
            let local = local.clone();
            self.replacement_cache.insert(cache_key, local.clone());
            return local;
        }

        let local = self.generate_name(imported);
        self.add_named_import(source, imported, &local);
        self.replacement_cache.insert(cache_key, local.clone());
        local
    }

    fn add_named_import(&mut self, source: &str, imported: &str, local: &str) {
        let key = (source.to_string(), imported.to_string(), local.to_string());
        if !self.addition_keys.insert(key) {
            return;
        }

        self.additions
            .entry(source.to_string())
            .or_default()
            .push(NamedImportAddition {
                imported: imported.to_string(),
                local: local.to_string(),
            });
        self.named_imports.insert(
            (source.to_string(), imported.to_string()),
            local.to_string(),
        );
    }

    fn generate_name(&mut self, base: &str) -> String {
        if !self.occupied_names.contains(base) {
            self.occupied_names.insert(base.to_string());
            return base.to_string();
        }

        let mut index = 1;
        loop {
            let candidate = format!("{base}_{index}");
            if !self.occupied_names.contains(&candidate) {
                self.occupied_names.insert(candidate.clone());
                return candidate;
            }
            index += 1;
        }
    }

    fn record_replaced_default_ref(&mut self, symbol_id: SymbolId) {
        *self.replaced_default_refs.entry(symbol_id).or_default() += 1;
    }

    fn default_can_be_removed(&self, symbol_id: SymbolId, scoping: &Scoping) -> bool {
        let replaced = self
            .replaced_default_refs
            .get(&symbol_id)
            .copied()
            .unwrap_or(0);
        replaced > 0 && replaced == scoping.get_resolved_reference_ids(symbol_id).len()
    }
}

struct IndirectCallReplacer<'a, 's, 'i> {
    ast: AstBuilder<'a>,
    scoping: &'s Scoping,
    imports: &'i mut ImportState,
}

impl<'a> VisitMut<'a> for IndirectCallReplacer<'a, '_, '_> {
    fn visit_call_expression(&mut self, call: &mut CallExpression<'a>) {
        walk_mut::walk_call_expression(self, call);

        let Some(target) = indirect_import_call_target(&call.callee, self.scoping, self.imports)
        else {
            return;
        };

        let local =
            self.imports
                .local_for_named_import(target.symbol_id, &target.source, &target.imported);
        self.imports.record_replaced_default_ref(target.symbol_id);
        call.callee = self
            .ast
            .expression_identifier(Span::default(), self.ast.ident(&local));
    }
}

struct IndirectCallTarget {
    symbol_id: SymbolId,
    source: String,
    imported: String,
}

fn indirect_import_call_target(
    callee: &Expression,
    scoping: &Scoping,
    imports: &ImportState,
) -> Option<IndirectCallTarget> {
    let Expression::SequenceExpression(sequence) = without_parentheses(callee) else {
        return None;
    };
    if sequence.expressions.len() != 2 || !is_zero_literal(&sequence.expressions[0]) {
        return None;
    }

    let Expression::StaticMemberExpression(member) = &sequence.expressions[1] else {
        return None;
    };
    let Expression::Identifier(object) = &member.object else {
        return None;
    };

    let symbol_id = object
        .reference_id
        .get()
        .and_then(|reference_id| scoping.get_reference(reference_id).symbol_id())?;
    let default_import = imports.default_imports.get(&symbol_id)?;

    Some(IndirectCallTarget {
        symbol_id,
        source: default_import.source.clone(),
        imported: member.property.name.as_str().to_string(),
    })
}

fn is_zero_literal(expression: &Expression) -> bool {
    matches!(without_parentheses(expression), Expression::NumericLiteral(literal) if literal.value == 0.0)
}

fn without_parentheses<'b, 'a>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            without_parentheses(&parenthesized.expression)
        }
        expression => expression,
    }
}

struct ImportRewriter<'a, 's> {
    ast: AstBuilder<'a>,
    imports: ImportState,
    scoping: &'s Scoping,
}

impl<'a> ImportRewriter<'a, '_> {
    fn transform_program(&mut self, program: &mut oxc_ast::ast::Program<'a>) {
        let old_body = program.body.take_in(self.ast);
        let mut new_body = self.ast.vec_with_capacity(old_body.len());

        for statement in old_body {
            match statement {
                oxc_ast::ast::Statement::ImportDeclaration(mut import) => {
                    if self.rewrite_import_declaration(&mut import) {
                        new_body.push(oxc_ast::ast::Statement::ImportDeclaration(import));
                    }
                }
                statement => new_body.push(statement),
            }
        }

        program.body = new_body;
    }

    fn rewrite_import_declaration(&mut self, import: &mut ImportDeclaration<'a>) -> bool {
        let source = import.source.value.as_str().to_string();
        let additions = self.imports.additions.remove(&source).unwrap_or_default();
        let Some(specifiers) = &mut import.specifiers else {
            return true;
        };

        specifiers.retain(|specifier| {
            !matches!(
                specifier,
                ImportDeclarationSpecifier::ImportDefaultSpecifier(default)
                    if default
                        .local
                        .symbol_id
                        .get()
                        .is_some_and(|symbol_id| self.imports.default_can_be_removed(symbol_id, self.scoping))
            )
        });

        if specifiers.iter().any(|specifier| {
            matches!(
                specifier,
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(_)
            )
        }) {
            return !specifiers.is_empty();
        }

        for addition in additions {
            specifiers.push(named_import_specifier(
                self.ast,
                &addition.imported,
                &addition.local,
            ));
        }

        !specifiers.is_empty()
    }
}

fn named_import_specifier<'a>(
    ast: AstBuilder<'a>,
    imported: &str,
    local: &str,
) -> ImportDeclarationSpecifier<'a> {
    ast.import_declaration_specifier_import_specifier(
        Span::default(),
        ast.module_export_name_identifier_name(Span::default(), ast.ident(imported)),
        ast.binding_identifier(Span::default(), ast.ident(local)),
        ImportOrExportKind::Value,
    )
}

fn collect_all_names(scoping: &Scoping) -> HashSet<String> {
    scoping
        .iter_bindings()
        .flat_map(|(_, bindings)| bindings.values().copied())
        .map(|symbol_id| scoping.symbol_name(symbol_id).to_string())
        .collect()
}

fn imported_name<'a>(imported: &'a oxc_ast::ast::ModuleExportName<'a>) -> Option<&'a str> {
    match imported {
        oxc_ast::ast::ModuleExportName::IdentifierName(identifier) => {
            Some(identifier.name.as_str())
        }
        oxc_ast::ast::ModuleExportName::IdentifierReference(identifier) => {
            Some(identifier.name.as_str())
        }
        oxc_ast::ast::ModuleExportName::StringLiteral(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn converts_indirect_calls_from_default_imports() {
        define_ast_inline_test(transform_ast)(
            "
import s from \"react\";

var countRef = (0, s.useRef)(0);
",
            "
import { useRef } from \"react\";
var countRef = useRef(0);
",
        );
    }

    #[test]
    fn reuses_existing_named_imports() {
        define_ast_inline_test(transform_ast)(
            "
import s from \"react\";
import { useRef } from \"react\";

var countRef = (0, s.useRef)(0);
",
            "
import { useRef } from \"react\";
var countRef = useRef(0);
",
        );
    }

    #[test]
    fn resolves_local_name_conflicts() {
        define_ast_inline_test(transform_ast)(
            "
import s from \"react\";

const fn = () => {
  const useRef = 1;
  (0, s.useRef)(0);
}
",
            "
import { useRef as useRef_1 } from \"react\";
const fn = () => {
  const useRef = 1;
  useRef_1(0);
};
",
        );
    }

    #[test]
    fn keeps_default_import_when_other_references_remain() {
        define_ast_inline_test(transform_ast)(
            "
import s from \"react\";

console.log(s);
var countRef = (0, s.useRef)(0);
",
            "
import s, { useRef } from \"react\";
console.log(s);
var countRef = useRef(0);
",
        );
    }

    #[test]
    fn ignores_shadowed_default_import_locals() {
        define_ast_inline_test(transform_ast)(
            "
import s from \"react\";

function fn(s) {
  return (0, s.useRef)(0);
}
",
            "
import s from \"react\";
function fn(s) {
  return (0, s.useRef)(0);
}
",
        );
    }
}
