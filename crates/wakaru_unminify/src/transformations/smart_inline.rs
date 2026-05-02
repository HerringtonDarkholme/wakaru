use oxc_allocator::{CloneIn, TakeIn};
use oxc_ast::{
    ast::{
        BindingPattern, Expression, Statement, VariableDeclaration, VariableDeclarationKind,
        VariableDeclarator,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_semantic::{Scoping, SemanticBuilder, SymbolId};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();
    let mut inliner = TempVariableInliner {
        ast: AstBuilder::new(source.allocator),
        scoping,
    };

    inliner.visit_program(&mut source.program);

    Ok(())
}

struct TempVariableInliner<'a> {
    ast: AstBuilder<'a>,
    scoping: Scoping,
}

impl<'a> VisitMut<'a> for TempVariableInliner<'a> {
    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        walk_mut::walk_statements(self, statements);
        self.inline_temp_variables(statements);
    }
}

impl<'a> TempVariableInliner<'a> {
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
}
