use std::collections::{HashMap, HashSet};

use oxc_allocator::{Box as OxcBox, CloneIn, TakeIn};
use oxc_ast::{
    ast::{
        BindingPattern, BindingProperty, BindingRestElement, Expression, IdentifierReference,
        PropertyKey, Statement, TSTypeAnnotation, VariableDeclaration, VariableDeclarationKind,
        VariableDeclarator,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_semantic::{ScopeId, Scoping, SemanticBuilder, SymbolId};
use oxc_span::Span;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();
    let mut array_destructurer = TempVariableInliner {
        ast: AstBuilder::new(source.allocator),
        scoping,
        reconstruct_arrays: true,
        reconstruct_objects: true,
        inline_globals: false,
        inline_temps: false,
    };
    array_destructurer.visit_program(&mut source.program);

    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();
    let mut inliner = TempVariableInliner {
        ast: AstBuilder::new(source.allocator),
        scoping,
        reconstruct_arrays: false,
        reconstruct_objects: false,
        inline_globals: true,
        inline_temps: true,
    };
    inliner.visit_program(&mut source.program);

    Ok(())
}

struct TempVariableInliner<'a> {
    ast: AstBuilder<'a>,
    scoping: Scoping,
    reconstruct_arrays: bool,
    reconstruct_objects: bool,
    inline_globals: bool,
    inline_temps: bool,
}

impl<'a> VisitMut<'a> for TempVariableInliner<'a> {
    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        walk_mut::walk_statements(self, statements);
        if self.reconstruct_arrays {
            self.reconstruct_array_destructuring(statements);
        }
        if self.reconstruct_objects {
            self.reconstruct_object_destructuring(statements);
        }
        if self.inline_globals {
            self.inline_global_aliases(statements);
        }
        if self.inline_temps {
            self.inline_temp_variables(statements);
        }
    }
}

impl<'a> TempVariableInliner<'a> {
    fn reconstruct_array_destructuring(
        &self,
        statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        if statements.len() < 2 {
            return;
        }

        let groups = self.array_destructuring_groups(statements);
        if groups.is_empty() {
            return;
        }

        let mut remove_statement = vec![false; statements.len()];
        for group in &groups {
            for access in &group.accesses {
                remove_statement[access.statement_index] = true;
            }
        }

        let old_statements = statements.take_in(self.ast);
        let mut new_statements = self.ast.vec_with_capacity(old_statements.len());

        for (index, statement) in old_statements.into_iter().enumerate() {
            for group in groups.iter().filter(|group| group.insert_index == index) {
                new_statements.push(self.array_destructuring_statement(group));
            }

            if !remove_statement[index] {
                new_statements.push(statement);
            }
        }

        *statements = new_statements;
    }

    fn array_destructuring_groups(
        &self,
        statements: &[Statement<'a>],
    ) -> Vec<ArrayDestructuringGroup<'a>> {
        let mut groups: Vec<ArrayDestructuringGroup<'a>> = Vec::new();

        for (statement_index, statement) in statements.iter().enumerate() {
            let Some(access) = self.array_index_access(statement_index, statement) else {
                continue;
            };

            let Some(group) = groups
                .iter_mut()
                .find(|group| group.object_name == access.object_name)
            else {
                groups.push(ArrayDestructuringGroup {
                    object_name: access.object_name.clone(),
                    object: access.object.clone_in(self.ast.allocator),
                    kind: access.kind,
                    span: access.declaration_span,
                    declarator_span: access.declarator_span,
                    insert_index: statement_index,
                    accesses: vec![access],
                    duplicate_index: false,
                });
                continue;
            };

            group.kind = most_restrictive_kind(group.kind, access.kind);
            group.span = merge_spans(group.span, access.declaration_span);
            group.duplicate_index |= group
                .accesses
                .iter()
                .any(|existing| existing.element_index == access.element_index);
            group.accesses.push(access);
        }

        groups
            .into_iter()
            .filter(|group| group.accesses.len() > 1 && !group.duplicate_index)
            .collect()
    }

    fn array_index_access(
        &self,
        statement_index: usize,
        statement: &Statement<'a>,
    ) -> Option<ArrayIndexAccess<'a>> {
        let Statement::VariableDeclaration(declaration) = statement else {
            return None;
        };
        if declaration.declarations.len() != 1 || !is_supported_declaration_kind(declaration.kind) {
            return None;
        }

        let declarator = &declaration.declarations[0];
        if !matches!(declarator.id, BindingPattern::BindingIdentifier(_)) {
            return None;
        }

        let Expression::ComputedMemberExpression(member) = declarator.init.as_ref()? else {
            return None;
        };
        let Expression::Identifier(object) = &member.object else {
            return None;
        };
        let Expression::NumericLiteral(property) = &member.expression else {
            return None;
        };

        let element_index = numeric_destructuring_index(property.value)?;

        Some(ArrayIndexAccess {
            statement_index,
            object_name: object.name.as_str().to_string(),
            object: member.object.clone_in(self.ast.allocator),
            element_index,
            binding: declarator.id.clone_in(self.ast.allocator),
            kind: declaration.kind,
            declaration_span: declaration.span,
            declarator_span: declarator.span,
        })
    }

    fn array_destructuring_statement(&self, group: &ArrayDestructuringGroup<'a>) -> Statement<'a> {
        let max_index = group
            .accesses
            .iter()
            .map(|access| access.element_index)
            .max()
            .unwrap_or(0);
        let mut elements = self.ast.vec_with_capacity(max_index + 1);

        for element_index in 0..=max_index {
            let binding = group
                .accesses
                .iter()
                .find(|access| access.element_index == element_index)
                .map(|access| access.binding.clone_in(self.ast.allocator));
            elements.push(binding);
        }

        let pattern = self.ast.binding_pattern_array_pattern(
            group.span,
            elements,
            None::<OxcBox<'a, BindingRestElement<'a>>>,
        );
        let declarator = self.ast.variable_declarator(
            group.declarator_span,
            group.kind,
            pattern,
            None::<OxcBox<'a, TSTypeAnnotation<'a>>>,
            Some(group.object.clone_in(self.ast.allocator)),
            false,
        );
        let mut declarations = self.ast.vec_with_capacity(1);
        declarations.push(declarator);

        Statement::VariableDeclaration(self.ast.alloc_variable_declaration(
            group.span,
            group.kind,
            declarations,
            false,
        ))
    }

    fn reconstruct_object_destructuring(
        &mut self,
        statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        if statements.len() < 2 {
            return;
        }

        let groups = self.object_destructuring_groups(statements);
        if groups.is_empty() {
            return;
        }

        let mut remove_statement = vec![false; statements.len()];
        let mut renames = HashMap::new();

        for group in &groups {
            for access in &group.accesses {
                remove_statement[access.statement_index] = true;
                if let Some(symbol_id) = access.binding_symbol_id {
                    renames.insert(symbol_id, access.local_name.clone());
                }
            }
        }

        rename_references(self.ast, &self.scoping, statements, &renames);

        let old_statements = statements.take_in(self.ast);
        let mut new_statements = self.ast.vec_with_capacity(old_statements.len());

        for (index, statement) in old_statements.into_iter().enumerate() {
            for group in groups.iter().filter(|group| group.insert_index == index) {
                new_statements.push(self.object_destructuring_statement(group));
            }

            if !remove_statement[index] {
                new_statements.push(statement);
            }
        }

        *statements = new_statements;
    }

    fn object_destructuring_groups(
        &mut self,
        statements: &[Statement<'a>],
    ) -> Vec<ObjectDestructuringGroup<'a>> {
        let mut groups: Vec<ObjectDestructuringGroup<'a>> = Vec::new();
        let mut side_effect_accesses = Vec::new();

        for (statement_index, statement) in statements.iter().enumerate() {
            if let Some(access) =
                self.object_property_access_declaration(statement_index, statement)
            {
                push_object_access_group(self.ast, &mut groups, access);
                continue;
            }

            if let Some(access) = self.object_property_access_expression(statement_index, statement)
            {
                side_effect_accesses.push(access);
            }
        }

        for access in side_effect_accesses {
            if groups
                .iter()
                .any(|group| group.object_name == access.object_name)
            {
                push_object_access_group(self.ast, &mut groups, access);
            }
        }

        for group in &mut groups {
            self.assign_object_destructuring_names(group);
        }

        groups
            .into_iter()
            .filter(|group| {
                group.accesses.len() > 1
                    && group
                        .accesses
                        .iter()
                        .any(|access| access.binding_symbol_id.is_some())
            })
            .collect()
    }

    fn object_property_access_declaration(
        &self,
        statement_index: usize,
        statement: &Statement<'a>,
    ) -> Option<ObjectPropertyAccess<'a>> {
        let Statement::VariableDeclaration(declaration) = statement else {
            return None;
        };
        if declaration.declarations.len() != 1 || !is_supported_declaration_kind(declaration.kind) {
            return None;
        }

        let declarator = &declaration.declarations[0];
        let BindingPattern::BindingIdentifier(binding) = &declarator.id else {
            return None;
        };
        let (object, property_name) = object_member_property(declarator.init.as_ref()?)?;
        let Expression::Identifier(object_identifier) = object else {
            return None;
        };
        let binding_symbol_id = binding.symbol_id.get()?;
        let scope_id = self.scoping.symbol_scope_id(binding_symbol_id);

        Some(ObjectPropertyAccess {
            statement_index,
            object_name: object_identifier.name.as_str().to_string(),
            object: object.clone_in(self.ast.allocator),
            property_name,
            binding_symbol_id: Some(binding_symbol_id),
            scope_id,
            local_name: String::new(),
            kind: declaration.kind,
            span: declaration.span,
            declarator_span: declarator.span,
        })
    }

    fn object_property_access_expression(
        &self,
        statement_index: usize,
        statement: &Statement<'a>,
    ) -> Option<ObjectPropertyAccess<'a>> {
        let Statement::ExpressionStatement(statement) = statement else {
            return None;
        };
        let (object, property_name) = object_member_property(&statement.expression)?;
        let Expression::Identifier(object_identifier) = object else {
            return None;
        };

        Some(ObjectPropertyAccess {
            statement_index,
            object_name: object_identifier.name.as_str().to_string(),
            object: object.clone_in(self.ast.allocator),
            property_name,
            binding_symbol_id: None,
            scope_id: self.scoping.root_scope_id(),
            local_name: String::new(),
            kind: VariableDeclarationKind::Const,
            span: statement.span,
            declarator_span: statement.span,
        })
    }

    fn assign_object_destructuring_names(&mut self, group: &mut ObjectDestructuringGroup<'a>) {
        let mut property_names = HashMap::<String, String>::new();

        for access in &mut group.accesses {
            if let Some(local_name) = property_names.get(&access.property_name) {
                access.local_name = local_name.clone();
                continue;
            }

            let local_name = self.generate_name(access.scope_id, &access.property_name);
            property_names.insert(access.property_name.clone(), local_name.clone());
            access.local_name = local_name;
        }
    }

    fn object_destructuring_statement(
        &self,
        group: &ObjectDestructuringGroup<'a>,
    ) -> Statement<'a> {
        let mut seen_properties = HashSet::new();
        let mut properties = self.ast.vec();

        for access in &group.accesses {
            if !seen_properties.insert(access.property_name.clone()) {
                continue;
            }

            properties
                .push(self.object_binding_property(&access.property_name, &access.local_name));
        }

        let pattern = self.ast.binding_pattern_object_pattern(
            group.span,
            properties,
            None::<OxcBox<'a, BindingRestElement<'a>>>,
        );
        let declarator = self.ast.variable_declarator(
            group.declarator_span,
            group.kind,
            pattern,
            None::<OxcBox<'a, TSTypeAnnotation<'a>>>,
            Some(group.object.clone_in(self.ast.allocator)),
            false,
        );
        let mut declarations = self.ast.vec_with_capacity(1);
        declarations.push(declarator);

        Statement::VariableDeclaration(self.ast.alloc_variable_declaration(
            group.span,
            group.kind,
            declarations,
            false,
        ))
    }

    fn object_binding_property(
        &self,
        property_name: &str,
        local_name: &str,
    ) -> BindingProperty<'a> {
        self.ast.binding_property(
            Span::default(),
            property_key(self.ast, property_name),
            self.ast
                .binding_pattern_binding_identifier(Span::default(), self.ast.ident(local_name)),
            property_name == local_name,
            false,
        )
    }

    fn inline_global_aliases(&self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        if statements.len() < 2 {
            return;
        }

        let mut remove_statement = vec![false; statements.len()];
        let mut renames = HashMap::new();

        for (index, statement) in statements.iter().enumerate() {
            let Some((symbol_id, global_name)) = self.global_alias(statement) else {
                continue;
            };

            if self
                .scoping
                .get_resolved_reference_ids(symbol_id)
                .is_empty()
            {
                continue;
            }

            remove_statement[index] = true;
            renames.insert(symbol_id, global_name);
        }

        if renames.is_empty() {
            return;
        }

        rename_references(self.ast, &self.scoping, statements, &renames);

        let old_statements = statements.take_in(self.ast);
        let mut new_statements = self.ast.vec_with_capacity(old_statements.len());

        for (index, statement) in old_statements.into_iter().enumerate() {
            if !remove_statement[index] {
                new_statements.push(statement);
            }
        }

        *statements = new_statements;
    }

    fn global_alias(&self, statement: &Statement<'a>) -> Option<(SymbolId, String)> {
        let declarator = single_const_declarator(statement)?;
        let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return None;
        };
        let Expression::Identifier(init) = declarator.init.as_ref()? else {
            return None;
        };

        let symbol_id = identifier.symbol_id.get()?;
        let scope_id = self.scoping.symbol_scope_id(symbol_id);
        let global_name = init.name.as_str();
        if !is_global_identifier(global_name) {
            return None;
        }
        if self
            .scoping
            .find_binding(scope_id, global_name.into())
            .is_some()
        {
            return None;
        }

        Some((symbol_id, global_name.to_string()))
    }

    fn generate_name(&mut self, scope_id: ScopeId, raw_base: &str) -> String {
        let base = if is_valid_binding_identifier(raw_base) {
            raw_base.to_string()
        } else {
            format!("_{raw_base}")
        };

        if !self.is_name_occupied(scope_id, &base) {
            return base;
        }

        let mut index = 1;
        loop {
            let candidate = format!("{base}_{index}");
            if !self.is_name_occupied(scope_id, &candidate) {
                return candidate;
            }
            index += 1;
        }
    }

    fn is_name_occupied(&self, scope_id: ScopeId, name: &str) -> bool {
        self.scoping.find_binding(scope_id, name.into()).is_some()
    }

    fn inline_temp_variables(&self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        if statements.len() < 2 {
            return;
        }

        let mut remove_statement = vec![false; statements.len()];

        for index in 1..statements.len() {
            let Some((previous_symbol_id, previous_init)) =
                self.single_use_const_initializer(&statements[index - 1])
            else {
                continue;
            };

            if current_initializer_symbol(&statements[index], &self.scoping)
                != Some(previous_symbol_id)
            {
                continue;
            }

            replace_single_const_initializer(&mut statements[index], previous_init);
            remove_statement[index - 1] = true;
        }

        if !remove_statement.iter().any(|remove| *remove) {
            return;
        }

        let old_statements = statements.take_in(self.ast);
        let mut new_statements = self.ast.vec_with_capacity(old_statements.len());

        for (index, statement) in old_statements.into_iter().enumerate() {
            if !remove_statement[index] {
                new_statements.push(statement);
            }
        }

        *statements = new_statements;
    }

    fn single_use_const_initializer(
        &self,
        statement: &Statement<'a>,
    ) -> Option<(SymbolId, Expression<'a>)> {
        let declarator = single_const_declarator(statement)?;
        let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return None;
        };
        let symbol_id = identifier.symbol_id.get()?;

        if self.scoping.get_resolved_reference_ids(symbol_id).len() > 1 {
            return None;
        }

        let init = declarator.init.as_ref()?;
        Some((symbol_id, init.clone_in(self.ast.allocator)))
    }
}

struct ArrayDestructuringGroup<'a> {
    object_name: String,
    object: Expression<'a>,
    kind: VariableDeclarationKind,
    span: Span,
    declarator_span: Span,
    insert_index: usize,
    accesses: Vec<ArrayIndexAccess<'a>>,
    duplicate_index: bool,
}

struct ArrayIndexAccess<'a> {
    statement_index: usize,
    object_name: String,
    object: Expression<'a>,
    element_index: usize,
    binding: BindingPattern<'a>,
    kind: VariableDeclarationKind,
    declaration_span: Span,
    declarator_span: Span,
}

struct ObjectDestructuringGroup<'a> {
    object_name: String,
    object: Expression<'a>,
    kind: VariableDeclarationKind,
    span: Span,
    declarator_span: Span,
    insert_index: usize,
    accesses: Vec<ObjectPropertyAccess<'a>>,
}

struct ObjectPropertyAccess<'a> {
    statement_index: usize,
    object_name: String,
    object: Expression<'a>,
    property_name: String,
    binding_symbol_id: Option<SymbolId>,
    scope_id: ScopeId,
    local_name: String,
    kind: VariableDeclarationKind,
    span: Span,
    declarator_span: Span,
}

fn push_object_access_group<'a>(
    ast: AstBuilder<'a>,
    groups: &mut Vec<ObjectDestructuringGroup<'a>>,
    access: ObjectPropertyAccess<'a>,
) {
    let Some(group) = groups
        .iter_mut()
        .find(|group| group.object_name == access.object_name)
    else {
        groups.push(ObjectDestructuringGroup {
            object_name: access.object_name.clone(),
            object: access.object.clone_in(ast.allocator),
            kind: access.kind,
            span: access.span,
            declarator_span: access.declarator_span,
            insert_index: access.statement_index,
            accesses: vec![access],
        });
        return;
    };

    group.kind = most_restrictive_kind(group.kind, access.kind);
    group.span = merge_spans(group.span, access.span);
    group.insert_index = group.insert_index.min(access.statement_index);
    group.accesses.push(access);
}

fn object_member_property<'b, 'a>(
    expression: &'b Expression<'a>,
) -> Option<(&'b Expression<'a>, String)> {
    match expression {
        Expression::StaticMemberExpression(member) => {
            Some((&member.object, member.property.name.as_str().to_string()))
        }
        Expression::ComputedMemberExpression(member) => {
            let Expression::StringLiteral(property) = &member.expression else {
                return None;
            };
            Some((&member.object, property.value.as_str().to_string()))
        }
        _ => None,
    }
}

fn property_key<'a>(ast: AstBuilder<'a>, property_name: &str) -> PropertyKey<'a> {
    if is_valid_binding_identifier(property_name) {
        return ast.property_key_static_identifier(Span::default(), ast.ident(property_name));
    }

    PropertyKey::StringLiteral(ast.alloc_string_literal(
        Span::default(),
        ast.str(property_name),
        None,
    ))
}

fn rename_references<'a>(
    ast: AstBuilder<'a>,
    scoping: &Scoping,
    statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    renames: &HashMap<SymbolId, String>,
) {
    if renames.is_empty() {
        return;
    }

    let mut renamer = ReferenceRenamer {
        ast,
        scoping,
        renames,
    };
    renamer.visit_statements(statements);
}

struct ReferenceRenamer<'a, 's> {
    ast: AstBuilder<'a>,
    scoping: &'s Scoping,
    renames: &'s HashMap<SymbolId, String>,
}

impl<'a> VisitMut<'a> for ReferenceRenamer<'a, '_> {
    fn visit_identifier_reference(&mut self, identifier: &mut IdentifierReference<'a>) {
        let Some(symbol_id) = identifier
            .reference_id
            .get()
            .and_then(|reference_id| self.scoping.get_reference(reference_id).symbol_id())
        else {
            return;
        };

        if let Some(new_name) = self.renames.get(&symbol_id) {
            identifier.name = self.ast.ident(new_name);
        }
    }
}

fn is_supported_declaration_kind(kind: VariableDeclarationKind) -> bool {
    matches!(
        kind,
        VariableDeclarationKind::Var
            | VariableDeclarationKind::Let
            | VariableDeclarationKind::Const
    )
}

fn is_global_identifier(name: &str) -> bool {
    matches!(
        name,
        "window"
            | "document"
            | "Function"
            | "Object"
            | "Array"
            | "String"
            | "Number"
            | "Boolean"
            | "Symbol"
            | "Date"
            | "RegExp"
            | "navigator"
            | "location"
            | "history"
    )
}

fn is_valid_binding_identifier(name: &str) -> bool {
    if is_reserved_identifier(name) {
        return false;
    }

    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    (first == '_' || first == '$' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphanumeric())
}

fn is_reserved_identifier(name: &str) -> bool {
    matches!(
        name,
        "arguments"
            | "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "null"
            | "return"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
    )
}

fn most_restrictive_kind(
    left: VariableDeclarationKind,
    right: VariableDeclarationKind,
) -> VariableDeclarationKind {
    if left == VariableDeclarationKind::Var || right == VariableDeclarationKind::Var {
        return VariableDeclarationKind::Var;
    }
    if left == VariableDeclarationKind::Let || right == VariableDeclarationKind::Let {
        return VariableDeclarationKind::Let;
    }
    VariableDeclarationKind::Const
}

fn numeric_destructuring_index(value: f64) -> Option<usize> {
    if !value.is_finite() || value < 0.0 || value > 10.0 || value.fract() != 0.0 {
        return None;
    }

    Some(value as usize)
}

fn merge_spans(left: Span, right: Span) -> Span {
    Span::new(left.start.min(right.start), left.end.max(right.end))
}

fn current_initializer_symbol(statement: &Statement, scoping: &Scoping) -> Option<SymbolId> {
    let declarator = single_const_declarator(statement)?;
    let Expression::Identifier(identifier) = declarator.init.as_ref()? else {
        return None;
    };

    identifier
        .reference_id
        .get()
        .and_then(|reference_id| scoping.get_reference(reference_id).symbol_id())
}

fn replace_single_const_initializer<'a>(statement: &mut Statement<'a>, init: Expression<'a>) {
    let Statement::VariableDeclaration(declaration) = statement else {
        return;
    };
    let Some(declarator) = declaration.declarations.get_mut(0) else {
        return;
    };

    declarator.init = Some(init);
}

fn single_const_declarator<'a>(statement: &'a Statement) -> Option<&'a VariableDeclarator<'a>> {
    let Statement::VariableDeclaration(declaration) = statement else {
        return None;
    };
    if !is_single_const_declaration(declaration) {
        return None;
    }

    declaration.declarations.first()
}

fn is_single_const_declaration(declaration: &VariableDeclaration) -> bool {
    declaration.kind == VariableDeclarationKind::Const && declaration.declarations.len() == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn inlines_adjacent_temp_variable_assignments() {
        define_ast_inline_test(transform_ast)(
            "
const t = e;
const n = t;

const o = 1;
const r = o;
const g = r;
",
            "
const n = e;
const g = 1;
",
        );
    }

    #[test]
    fn does_not_inline_when_temp_is_used_more_than_once() {
        define_ast_inline_test(transform_ast)(
            "
const t = e;
const n = t;
const o = t;
",
            "
const t = e;
const n = t;
const o = t;
",
        );
    }

    #[test]
    fn inlines_inside_block_statement_lists() {
        define_ast_inline_test(transform_ast)(
            "
function foo() {
  const t = e;
  const n = t;
  return n;
}
",
            "
function foo() {
  const n = e;
  return n;
}
",
        );
    }

    #[test]
    fn inlines_global_identifier_aliases() {
        define_ast_inline_test(transform_ast)(
            "
const w = window;
const d = document;
const c = d.createElement('canvas');
",
            "
const w = window;
const c = document.createElement(\"canvas\");
",
        );
    }

    #[test]
    fn does_not_inline_shadowed_global_identifier_aliases() {
        define_ast_inline_test(transform_ast)(
            "
const document = 1;
const d = document.toFixed();
const c = d.split('');
",
            "
const document = 1;
const d = document.toFixed();
const c = d.split(\"\");
",
        );
    }

    #[test]
    fn reconstructs_array_destructuring_from_index_accesses() {
        define_ast_inline_test(transform_ast)(
            "
const t = e[0];
const n = e[1];
const r = e[2];
console.log(t, n, r);
",
            "
const [t, n, r] = e;
console.log(t, n, r);
",
        );
    }

    #[test]
    fn reconstructs_array_destructuring_with_gaps() {
        define_ast_inline_test(transform_ast)(
            "
const t = e[1];
const n = e[2];
const r = e[4];
const g = e[99];
console.log(t, n, r, g);
",
            "
const [, t, n, , r] = e;
const g = e[99];
console.log(t, n, r, g);
",
        );
    }

    #[test]
    fn reconstructs_array_destructuring_after_temp_inlining() {
        define_ast_inline_test(transform_ast)(
            "
const e = source;
const t = e[0];
const n = e[1];
const r = e[2];
console.log(t, n, r);
",
            "
const [t, n, r] = source;
console.log(t, n, r);
",
        );
    }

    #[test]
    fn reconstructs_object_destructuring_from_property_accesses() {
        define_ast_inline_test(transform_ast)(
            "
const t = e.x;
const n = e.y;
const r = e.color;
e.type;
console.log(t, n, r);
",
            "
const { x, y, color, type } = e;
console.log(x, y, color);
",
        );
    }

    #[test]
    fn reconstructs_object_destructuring_with_invalid_properties_and_conflicts() {
        define_ast_inline_test(transform_ast)(
            "
const color = 1;
const t = e['color'];
const n = e['2d'];
console.log(t, n, color);
",
            "
const color = 1;
const { color: color_1, \"2d\": _2d } = e;
console.log(color_1, _2d, color);
",
        );
    }

    #[test]
    fn reconstructs_object_destructuring_after_temp_inlining() {
        define_ast_inline_test(transform_ast)(
            "
const e = source;
const t = e.x;
const n = e.y;
const r = e.color;
console.log(t, n, r);
",
            "
const { x, y, color } = source;
console.log(x, y, color);
",
        );
    }
}
