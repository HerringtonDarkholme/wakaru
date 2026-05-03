use std::collections::{HashMap, HashSet};

use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{
        Argument, BindingPattern, BindingProperty, CallExpression, Expression, ImportDeclaration,
        ImportDeclarationSpecifier, ImportOrExportKind, PropertyKey, Statement,
        VariableDeclaration, VariableDeclarationKind, VariableDeclarator,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk, walk_mut, Visit, VisitMut};
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
    namespace_imports: HashMap<SymbolId, NamespaceImport>,
    named_imports: HashMap<(String, String), String>,
    additions: HashMap<String, Vec<NamedImportAddition>>,
    addition_keys: HashSet<(String, String, String)>,
    occupied_names: HashSet<String>,
    replaced_default_refs: HashMap<SymbolId, usize>,
    replaced_namespace_refs: HashMap<SymbolId, usize>,
    replacement_cache: HashMap<(SymbolId, String), String>,
    require_bindings: HashMap<SymbolId, RequireBinding>,
    require_binding_order: Vec<SymbolId>,
    require_destructures: Vec<RequireDestructure>,
    require_destructure_additions: HashMap<(usize, usize), Vec<RequirePropertyAddition>>,
    require_insertions: HashMap<SymbolId, RequireDestructureInsertion>,
    require_replacement_cache: HashMap<(SymbolId, String), String>,
    local_bindings: HashMap<SymbolId, LocalNamespaceBinding>,
    local_destructures: Vec<LocalDestructure>,
    local_insertions: HashMap<SymbolId, LocalDestructureInsertion>,
    local_replacement_cache: HashMap<(SymbolId, String), String>,
}

struct DefaultImport {
    source: String,
}

struct NamespaceImport {
    source: String,
}

struct NamedImportAddition {
    imported: String,
    local: String,
}

struct RequireBinding {
    local: String,
    statement_index: usize,
    span: Span,
}

struct RequireDestructure {
    symbol_id: SymbolId,
    statement_index: usize,
    declarator_index: usize,
    span: Span,
    properties: HashMap<String, String>,
}

#[derive(Clone)]
struct RequirePropertyAddition {
    imported: String,
    local: String,
}

struct RequireDestructureInsertion {
    object_local: String,
    properties: Vec<RequirePropertyAddition>,
}

struct LocalNamespaceBinding {
    local: String,
    span: Span,
}

struct LocalDestructure {
    symbol_id: SymbolId,
    span: Span,
    properties: HashMap<String, String>,
}

struct LocalDestructureInsertion {
    object_local: String,
    properties: Vec<RequirePropertyAddition>,
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

        let mut local_collector = LocalNamespaceCollector {
            scoping,
            local_bindings: &mut state.local_bindings,
            local_destructures: &mut state.local_destructures,
        };
        local_collector.visit_program(program);

        for (statement_index, statement) in program.body.iter().enumerate() {
            let Statement::VariableDeclaration(declaration) = statement else {
                continue;
            };

            state.collect_require_bindings(statement_index, declaration);
        }

        for (statement_index, statement) in program.body.iter().enumerate() {
            let Statement::VariableDeclaration(declaration) = statement else {
                continue;
            };

            state.collect_require_destructures(statement_index, declaration, scoping);
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
                    if let Some(symbol_id) = namespace.local.symbol_id.get() {
                        self.namespace_imports.insert(
                            symbol_id,
                            NamespaceImport {
                                source: source.clone(),
                            },
                        );
                    }
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

    fn record_replaced_namespace_ref(&mut self, symbol_id: SymbolId) {
        *self.replaced_namespace_refs.entry(symbol_id).or_default() += 1;
    }

    fn default_can_be_removed(&self, symbol_id: SymbolId, scoping: &Scoping) -> bool {
        let replaced = self
            .replaced_default_refs
            .get(&symbol_id)
            .copied()
            .unwrap_or(0);
        replaced > 0 && replaced == scoping.get_resolved_reference_ids(symbol_id).len()
    }

    fn namespace_can_be_removed(&self, symbol_id: SymbolId, scoping: &Scoping) -> bool {
        let replaced = self
            .replaced_namespace_refs
            .get(&symbol_id)
            .copied()
            .unwrap_or(0);
        replaced > 0 && replaced == scoping.get_resolved_reference_ids(symbol_id).len()
    }

    fn collect_require_bindings(
        &mut self,
        statement_index: usize,
        declaration: &VariableDeclaration,
    ) {
        for declarator in &declaration.declarations {
            let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
                continue;
            };
            let Some(init) = &declarator.init else {
                continue;
            };
            if !is_require_call(init) {
                continue;
            }
            let Some(symbol_id) = identifier.symbol_id.get() else {
                continue;
            };

            self.require_bindings.insert(
                symbol_id,
                RequireBinding {
                    local: identifier.name.as_str().to_string(),
                    statement_index,
                    span: declaration.span,
                },
            );
            self.require_binding_order.push(symbol_id);
        }
    }

    fn collect_require_destructures(
        &mut self,
        statement_index: usize,
        declaration: &VariableDeclaration,
        scoping: &Scoping,
    ) {
        for (declarator_index, declarator) in declaration.declarations.iter().enumerate() {
            let BindingPattern::ObjectPattern(pattern) = &declarator.id else {
                continue;
            };
            let Some(Expression::Identifier(init)) = &declarator.init else {
                continue;
            };
            let Some(symbol_id) = init
                .reference_id
                .get()
                .and_then(|reference_id| scoping.get_reference(reference_id).symbol_id())
            else {
                continue;
            };
            if !self.require_bindings.contains_key(&symbol_id) {
                continue;
            }

            let mut properties = HashMap::new();
            for property in &pattern.properties {
                let Some((imported, local)) = binding_property_names(property) else {
                    continue;
                };
                properties.insert(imported.to_string(), local.to_string());
            }

            self.require_destructures.push(RequireDestructure {
                symbol_id,
                statement_index,
                declarator_index,
                span: declaration.span,
                properties,
            });
        }
    }

    fn local_for_required_property(
        &mut self,
        symbol_id: SymbolId,
        imported: &str,
        call_span: Span,
    ) -> Option<String> {
        let cache_key = (symbol_id, imported.to_string());
        if let Some(local) = self.require_replacement_cache.get(&cache_key) {
            return Some(local.clone());
        }

        if let Some(local) = self.existing_required_property_local(symbol_id, imported, call_span) {
            self.require_replacement_cache
                .insert(cache_key, local.clone());
            return Some(local);
        }

        let local = self.generate_name(imported);
        let addition = RequirePropertyAddition {
            imported: imported.to_string(),
            local: local.clone(),
        };

        if let Some((statement_index, declarator_index)) =
            self.existing_required_destructure_destination(symbol_id, call_span)
        {
            self.require_destructure_additions
                .entry((statement_index, declarator_index))
                .or_default()
                .push(addition);
        } else {
            let require_binding = self.require_bindings.get(&symbol_id)?;
            self.require_insertions
                .entry(symbol_id)
                .or_insert_with(|| RequireDestructureInsertion {
                    object_local: require_binding.local.clone(),
                    properties: Vec::new(),
                })
                .properties
                .push(addition);
        }

        self.require_replacement_cache
            .insert(cache_key, local.clone());
        Some(local)
    }

    fn existing_required_property_local(
        &self,
        symbol_id: SymbolId,
        imported: &str,
        call_span: Span,
    ) -> Option<String> {
        self.require_destructures
            .iter()
            .filter(|destructure| {
                destructure.symbol_id == symbol_id
                    && self.is_between_require_and_call(destructure.span, symbol_id, call_span)
            })
            .find_map(|destructure| destructure.properties.get(imported).cloned())
    }

    fn existing_required_destructure_destination(
        &self,
        symbol_id: SymbolId,
        call_span: Span,
    ) -> Option<(usize, usize)> {
        self.require_destructures
            .iter()
            .find(|destructure| {
                destructure.symbol_id == symbol_id
                    && self.is_between_require_and_call(destructure.span, symbol_id, call_span)
            })
            .map(|destructure| (destructure.statement_index, destructure.declarator_index))
    }

    fn is_between_require_and_call(
        &self,
        span: Span,
        symbol_id: SymbolId,
        call_span: Span,
    ) -> bool {
        self.require_bindings
            .get(&symbol_id)
            .is_some_and(|binding| span.start > binding.span.end && span.end < call_span.start)
    }

    fn local_for_local_property(
        &mut self,
        symbol_id: SymbolId,
        imported: &str,
        call_span: Span,
    ) -> Option<String> {
        let cache_key = (symbol_id, imported.to_string());
        if let Some(local) = self.local_replacement_cache.get(&cache_key) {
            return Some(local.clone());
        }

        if let Some(local) = self.existing_local_property_local(symbol_id, imported, call_span) {
            self.local_replacement_cache
                .insert(cache_key, local.clone());
            return Some(local);
        }

        let local = self.generate_name(imported);
        let addition = RequirePropertyAddition {
            imported: imported.to_string(),
            local: local.clone(),
        };
        let binding = self.local_bindings.get(&symbol_id)?;
        self.local_insertions
            .entry(symbol_id)
            .or_insert_with(|| LocalDestructureInsertion {
                object_local: binding.local.clone(),
                properties: Vec::new(),
            })
            .properties
            .push(addition);

        self.local_replacement_cache
            .insert(cache_key, local.clone());
        Some(local)
    }

    fn existing_local_property_local(
        &self,
        symbol_id: SymbolId,
        imported: &str,
        call_span: Span,
    ) -> Option<String> {
        self.local_destructures
            .iter()
            .filter(|destructure| {
                destructure.symbol_id == symbol_id
                    && self.is_between_local_and_call(destructure.span, symbol_id, call_span)
            })
            .find_map(|destructure| destructure.properties.get(imported).cloned())
    }

    fn is_between_local_and_call(&self, span: Span, symbol_id: SymbolId, call_span: Span) -> bool {
        self.local_bindings
            .get(&symbol_id)
            .is_some_and(|binding| span.start > binding.span.end && span.end < call_span.start)
    }
}

struct LocalNamespaceCollector<'s, 'f, 'd> {
    scoping: &'s Scoping,
    local_bindings: &'f mut HashMap<SymbolId, LocalNamespaceBinding>,
    local_destructures: &'d mut Vec<LocalDestructure>,
}

impl<'a> Visit<'a> for LocalNamespaceCollector<'_, '_, '_> {
    fn visit_variable_declarator(&mut self, declarator: &VariableDeclarator<'a>) {
        self.collect_local_binding(declarator);
        self.collect_local_destructure(declarator);
        walk::walk_variable_declarator(self, declarator);
    }
}

impl LocalNamespaceCollector<'_, '_, '_> {
    fn collect_local_binding(&mut self, declarator: &VariableDeclarator) {
        let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return;
        };
        if declarator.init.is_none() {
            return;
        }
        let Some(symbol_id) = identifier.symbol_id.get() else {
            return;
        };

        self.local_bindings.insert(
            symbol_id,
            LocalNamespaceBinding {
                local: identifier.name.as_str().to_string(),
                span: declarator.span,
            },
        );
    }

    fn collect_local_destructure(&mut self, declarator: &VariableDeclarator) {
        let BindingPattern::ObjectPattern(pattern) = &declarator.id else {
            return;
        };
        let Some(Expression::Identifier(init)) = &declarator.init else {
            return;
        };
        let Some(symbol_id) = init
            .reference_id
            .get()
            .and_then(|reference_id| self.scoping.get_reference(reference_id).symbol_id())
        else {
            return;
        };
        if !self.local_bindings.contains_key(&symbol_id) {
            return;
        }

        let mut properties = HashMap::new();
        for property in &pattern.properties {
            let Some((imported, local)) = binding_property_names(property) else {
                continue;
            };
            properties.insert(imported.to_string(), local.to_string());
        }

        self.local_destructures.push(LocalDestructure {
            symbol_id,
            span: declarator.span,
            properties,
        });
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

        let Some(target) =
            indirect_import_call_target(&call.callee, call.span, self.scoping, self.imports)
        else {
            return;
        };

        let local = match target {
            IndirectCallTarget::Import {
                symbol_id,
                source,
                imported,
                binding,
            } => {
                let local = self
                    .imports
                    .local_for_named_import(symbol_id, &source, &imported);
                match binding {
                    ImportBinding::Default => self.imports.record_replaced_default_ref(symbol_id),
                    ImportBinding::Namespace => {
                        self.imports.record_replaced_namespace_ref(symbol_id)
                    }
                }
                local
            }
            IndirectCallTarget::Require {
                symbol_id,
                imported,
            } => {
                let Some(local) = self
                    .imports
                    .local_for_required_property(symbol_id, &imported, call.span)
                else {
                    return;
                };
                local
            }
            IndirectCallTarget::Local {
                symbol_id,
                imported,
            } => {
                let Some(local) = self
                    .imports
                    .local_for_local_property(symbol_id, &imported, call.span)
                else {
                    return;
                };
                local
            }
        };
        call.callee = self
            .ast
            .expression_identifier(Span::default(), self.ast.ident(&local));
    }
}

enum IndirectCallTarget {
    Import {
        symbol_id: SymbolId,
        source: String,
        imported: String,
        binding: ImportBinding,
    },
    Require {
        symbol_id: SymbolId,
        imported: String,
    },
    Local {
        symbol_id: SymbolId,
        imported: String,
    },
}

enum ImportBinding {
    Default,
    Namespace,
}

fn indirect_import_call_target(
    callee: &Expression,
    call_span: Span,
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

    if let Some(default_import) = imports.default_imports.get(&symbol_id) {
        return Some(IndirectCallTarget::Import {
            symbol_id,
            source: default_import.source.clone(),
            imported: member.property.name.as_str().to_string(),
            binding: ImportBinding::Default,
        });
    }

    if let Some(namespace_import) = imports.namespace_imports.get(&symbol_id) {
        return Some(IndirectCallTarget::Import {
            symbol_id,
            source: namespace_import.source.clone(),
            imported: member.property.name.as_str().to_string(),
            binding: ImportBinding::Namespace,
        });
    }

    if imports
        .require_bindings
        .get(&symbol_id)
        .is_some_and(|binding| call_span.start > binding.span.end)
    {
        return Some(IndirectCallTarget::Require {
            symbol_id,
            imported: member.property.name.as_str().to_string(),
        });
    }

    if imports
        .local_bindings
        .get(&symbol_id)
        .is_some_and(|binding| call_span.start > binding.span.end)
    {
        return Some(IndirectCallTarget::Local {
            symbol_id,
            imported: member.property.name.as_str().to_string(),
        });
    }

    None
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
        self.transform_statement_list(&mut program.body, true);
    }

    fn transform_statement_list(
        &mut self,
        statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
        is_program_body: bool,
    ) {
        let old_body = statements.take_in(self.ast);
        let mut new_body = self.ast.vec_with_capacity(old_body.len());

        for (statement_index, statement) in old_body.into_iter().enumerate() {
            match statement {
                oxc_ast::ast::Statement::ImportDeclaration(mut import) if is_program_body => {
                    let extra_import = self.rewrite_import_declaration(&mut import);
                    if !import
                        .specifiers
                        .as_ref()
                        .is_some_and(|specifiers| specifiers.is_empty())
                    {
                        new_body.push(oxc_ast::ast::Statement::ImportDeclaration(import));
                    }
                    if let Some(extra_import) = extra_import {
                        new_body.push(extra_import);
                    }
                }
                oxc_ast::ast::Statement::VariableDeclaration(mut declaration) => {
                    if is_program_body {
                        self.rewrite_require_destructuring(statement_index, &mut declaration);
                    }
                    let mut statement = oxc_ast::ast::Statement::VariableDeclaration(declaration);
                    walk_mut::walk_statement(self, &mut statement);
                    let local_symbols = local_binding_symbol_ids(&statement);
                    new_body.push(statement);
                    self.push_local_insertions(local_symbols, &mut new_body);
                    if is_program_body {
                        self.push_require_insertions(statement_index, &mut new_body);
                    }
                }
                mut statement => {
                    walk_mut::walk_statement(self, &mut statement);
                    new_body.push(statement);
                    if is_program_body {
                        self.push_require_insertions(statement_index, &mut new_body);
                    }
                }
            }
        }

        *statements = new_body;
    }

    fn rewrite_import_declaration(
        &mut self,
        import: &mut ImportDeclaration<'a>,
    ) -> Option<Statement<'a>> {
        let source = import.source.value.as_str().to_string();
        let additions = self.imports.additions.remove(&source).unwrap_or_default();
        let Some(specifiers) = &mut import.specifiers else {
            return None;
        };

        specifiers.retain(|specifier| {
            let removable_default = matches!(
                specifier,
                ImportDeclarationSpecifier::ImportDefaultSpecifier(default)
                    if default
                        .local
                        .symbol_id
                        .get()
                        .is_some_and(|symbol_id| self.imports.default_can_be_removed(symbol_id, self.scoping))
            );
            let removable_namespace = matches!(
                specifier,
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace)
                    if namespace
                        .local
                        .symbol_id
                        .get()
                        .is_some_and(|symbol_id| self.imports.namespace_can_be_removed(symbol_id, self.scoping))
            );

            !(removable_default || removable_namespace)
        });

        if specifiers.iter().any(|specifier| {
            matches!(
                specifier,
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(_)
            )
        }) {
            return (!additions.is_empty())
                .then(|| named_import_statement(self.ast, &source, additions));
        }

        for addition in additions {
            specifiers.push(named_import_specifier(
                self.ast,
                &addition.imported,
                &addition.local,
            ));
        }

        None
    }

    fn rewrite_require_destructuring(
        &mut self,
        statement_index: usize,
        declaration: &mut VariableDeclaration<'a>,
    ) {
        for (declarator_index, declarator) in declaration.declarations.iter_mut().enumerate() {
            let Some(additions) = self
                .imports
                .require_destructure_additions
                .remove(&(statement_index, declarator_index))
            else {
                continue;
            };
            let BindingPattern::ObjectPattern(pattern) = &mut declarator.id else {
                continue;
            };

            for addition in additions {
                pattern.properties.push(require_binding_property(
                    self.ast,
                    &addition.imported,
                    &addition.local,
                ));
            }
        }
    }

    fn push_require_insertions(
        &mut self,
        statement_index: usize,
        new_body: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        for symbol_id in self.imports.require_binding_order.clone() {
            let Some(binding) = self.imports.require_bindings.get(&symbol_id) else {
                continue;
            };
            if binding.statement_index != statement_index {
                continue;
            }
            let Some(insertion) = self.imports.require_insertions.remove(&symbol_id) else {
                continue;
            };

            new_body.push(require_destructure_statement(
                self.ast,
                &insertion.object_local,
                insertion.properties,
            ));
        }
    }

    fn push_local_insertions(
        &mut self,
        symbol_ids: Vec<SymbolId>,
        new_body: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        for symbol_id in symbol_ids {
            let Some(insertion) = self.imports.local_insertions.remove(&symbol_id) else {
                continue;
            };
            new_body.push(require_destructure_statement(
                self.ast,
                &insertion.object_local,
                insertion.properties,
            ));
        }
    }
}

impl<'a> VisitMut<'a> for ImportRewriter<'a, '_> {
    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        self.transform_statement_list(statements, false);
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

fn named_import_statement<'a>(
    ast: AstBuilder<'a>,
    source: &str,
    additions: Vec<NamedImportAddition>,
) -> Statement<'a> {
    let mut specifiers = ast.vec_with_capacity(additions.len());
    for addition in additions {
        specifiers.push(named_import_specifier(
            ast,
            &addition.imported,
            &addition.local,
        ));
    }

    Statement::ImportDeclaration(ast.alloc_import_declaration(
        Span::default(),
        Some(specifiers),
        ast.string_literal(Span::default(), ast.str(source), None),
        None,
        None::<oxc_allocator::Box<'a, oxc_ast::ast::WithClause<'a>>>,
        ImportOrExportKind::Value,
    ))
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

fn is_require_call(expression: &Expression) -> bool {
    let Expression::CallExpression(call) = without_parentheses(expression) else {
        return false;
    };
    if call.arguments.len() != 1 {
        return false;
    }
    if !matches!(&call.callee, Expression::Identifier(identifier) if identifier.name == "require") {
        return false;
    }

    matches!(
        call.arguments.first(),
        Some(Argument::StringLiteral(_) | Argument::NumericLiteral(_))
    )
}

fn binding_property_names<'a>(property: &'a BindingProperty<'a>) -> Option<(&'a str, &'a str)> {
    let PropertyKey::StaticIdentifier(key) = &property.key else {
        return None;
    };
    let BindingPattern::BindingIdentifier(value) = &property.value else {
        return None;
    };

    Some((key.name.as_str(), value.name.as_str()))
}

fn local_binding_symbol_ids(statement: &Statement) -> Vec<SymbolId> {
    let Statement::VariableDeclaration(declaration) = statement else {
        return Vec::new();
    };

    declaration
        .declarations
        .iter()
        .filter_map(|declarator| {
            let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
                return None;
            };
            identifier.symbol_id.get()
        })
        .collect()
}

fn require_destructure_statement<'a>(
    ast: AstBuilder<'a>,
    object_local: &str,
    additions: Vec<RequirePropertyAddition>,
) -> Statement<'a> {
    let mut properties = ast.vec_with_capacity(additions.len());
    for addition in additions {
        properties.push(require_binding_property(
            ast,
            &addition.imported,
            &addition.local,
        ));
    }

    let mut declarations = ast.vec_with_capacity(1);
    declarations.push(ast.variable_declarator(
        Span::default(),
        VariableDeclarationKind::Const,
        ast.binding_pattern_object_pattern(
            Span::default(),
            properties,
            None::<oxc_allocator::Box<'a, oxc_ast::ast::BindingRestElement<'a>>>,
        ),
        None::<oxc_allocator::Box<'a, oxc_ast::ast::TSTypeAnnotation<'a>>>,
        Some(ast.expression_identifier(Span::default(), ast.ident(object_local))),
        false,
    ));

    Statement::VariableDeclaration(ast.alloc_variable_declaration(
        Span::default(),
        VariableDeclarationKind::Const,
        declarations,
        false,
    ))
}

fn require_binding_property<'a>(
    ast: AstBuilder<'a>,
    imported: &str,
    local: &str,
) -> BindingProperty<'a> {
    ast.binding_property(
        Span::default(),
        ast.property_key_static_identifier(Span::default(), ast.ident(imported)),
        ast.binding_pattern_binding_identifier(Span::default(), ast.ident(local)),
        imported == local,
        false,
    )
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
    fn converts_indirect_calls_from_namespace_imports() {
        define_ast_inline_test(transform_ast)(
            "
import * as $ from \"react/jsx-runtime\";

i = (0, $.jsx)(Z.Suspense, {
  fallback: children,
  children: r
});
",
            "
import { jsx } from \"react/jsx-runtime\";
i = jsx(Z.Suspense, {
  fallback: children,
  children: r
});
",
        );
    }

    #[test]
    fn keeps_namespace_import_when_other_references_remain() {
        define_ast_inline_test(transform_ast)(
            "
import * as $ from \"react/jsx-runtime\";

console.log($);
i = (0, $.jsx)(Z.Suspense, {
  fallback: children,
  children: r
});
",
            "
import * as $ from \"react/jsx-runtime\";
import { jsx } from \"react/jsx-runtime\";
console.log($);
i = jsx(Z.Suspense, {
  fallback: children,
  children: r
});
",
        );
    }

    #[test]
    fn converts_indirect_calls_from_factory_created_namespaces() {
        define_ast_inline_test(transform_ast)(
            "
import { t as t_7 } from \"./jsx-runtime-ebkFq_df.js\";

function render() {
  var $ = t_7();
  return (0, $.jsx)(C_7.Provider, {
    value: r,
    children
  });
}
",
            "
import { t as t_7 } from \"./jsx-runtime-ebkFq_df.js\";
function render() {
  var $ = t_7();
  const { jsx } = $;
  return jsx(C_7.Provider, {
    value: r,
    children
  });
}
",
        );
    }

    #[test]
    fn converts_indirect_calls_from_local_object_bindings() {
        define_ast_inline_test(transform_ast)(
            "
const namespace = getNamespace();

function render() {
  return (0, namespace.render)(value);
}
",
            "
const namespace = getNamespace();
const { render: render_1 } = namespace;
function render() {
  return render_1(value);
}
",
        );
    }

    #[test]
    fn reuses_existing_factory_namespace_destructuring() {
        define_ast_inline_test(transform_ast)(
            "
import { t as t_7 } from \"./jsx-runtime-ebkFq_df.js\";

function render() {
  var $ = t_7();
  const { jsx: runtimeJsx } = $;
  return (0, $.jsx)(C_7.Provider, {
    value: r,
    children
  });
}
",
            "
import { t as t_7 } from \"./jsx-runtime-ebkFq_df.js\";
function render() {
  var $ = t_7();
  const { jsx: runtimeJsx } = $;
  return runtimeJsx(C_7.Provider, {
    value: r,
    children
  });
}
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

    #[test]
    fn inserts_destructuring_for_required_modules() {
        define_ast_inline_test(transform_ast)(
            "
const s = require(\"react\");

var countRef = (0, s.useRef)(0);
",
            "
const s = require(\"react\");
const { useRef } = s;
var countRef = useRef(0);
",
        );
    }

    #[test]
    fn extends_existing_required_module_destructuring() {
        define_ast_inline_test(transform_ast)(
            "
const s = require(\"react\");
const { useRef } = s;

var countRef = (0, s.useRef)(0);
var secondRef = (0, s.useMemo)(() => {}, []);
",
            "
const s = require(\"react\");
const { useRef, useMemo } = s;
var countRef = useRef(0);
var secondRef = useMemo(() => {}, []);
",
        );
    }

    #[test]
    fn ignores_required_module_destructuring_declared_after_call() {
        define_ast_inline_test(transform_ast)(
            "
const s = require(\"react\");

var countRef = (0, s.useRef)(0);

const { useRef } = s;
",
            "
const s = require(\"react\");
const { useRef: useRef_1 } = s;
var countRef = useRef_1(0);
const { useRef } = s;
",
        );
    }

    #[test]
    fn coordinates_names_between_required_and_imported_modules() {
        define_ast_inline_test(transform_ast)(
            "
import p from \"r2\";

const s = require(\"react\");

var countRef = (0, s.useRef)(0);
var secondRef = (0, p.useRef)(0);
",
            "
import { useRef as useRef_1 } from \"r2\";
const s = require(\"react\");
const { useRef } = s;
var countRef = useRef(0);
var secondRef = useRef_1(0);
",
        );
    }

    #[test]
    fn inserts_destructuring_for_multiple_required_modules() {
        define_ast_inline_test(transform_ast)(
            "
const s = require(\"react\");
const t = require(9527);

var countRef = (0, s.useRef)(0);
var secondRef = (0, t.useRef)(0);
var thirdRef = (0, t.useRef)(0);
",
            "
const s = require(\"react\");
const { useRef } = s;
const t = require(9527);
const { useRef: useRef_1 } = t;
var countRef = useRef(0);
var secondRef = useRef_1(0);
var thirdRef = useRef_1(0);
",
        );
    }
}
