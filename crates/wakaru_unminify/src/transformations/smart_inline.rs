use oxc_allocator::{Box as OxcBox, CloneIn, TakeIn};
use oxc_ast::{
    ast::{
        BindingPattern, BindingRestElement, Expression, Statement, TSTypeAnnotation,
        VariableDeclaration, VariableDeclarationKind, VariableDeclarator,
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
    let mut array_destructurer = TempVariableInliner {
        ast: AstBuilder::new(source.allocator),
        scoping,
        reconstruct_arrays: true,
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
        inline_temps: true,
    };
    inliner.visit_program(&mut source.program);

    Ok(())
}

struct TempVariableInliner<'a> {
    ast: AstBuilder<'a>,
    scoping: Scoping,
    reconstruct_arrays: bool,
    inline_temps: bool,
}

impl<'a> VisitMut<'a> for TempVariableInliner<'a> {
    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        walk_mut::walk_statements(self, statements);
        if self.reconstruct_arrays {
            self.reconstruct_array_destructuring(statements);
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

fn is_supported_declaration_kind(kind: VariableDeclarationKind) -> bool {
    matches!(
        kind,
        VariableDeclarationKind::Var
            | VariableDeclarationKind::Let
            | VariableDeclarationKind::Const
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
}
