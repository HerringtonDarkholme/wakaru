use std::collections::HashMap;

use oxc_ast::{ast::Expression, AstBuilder};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_semantic::{ScopeId, SemanticBuilder};
use oxc_span::GetSpan;
use oxc_syntax::{node::NodeId, operator::UnaryOperator};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let semantic = SemanticBuilder::new().build(&source.program).semantic;
    let (scoping, nodes) = semantic.into_scoping_and_nodes();
    let node_scope_ids = nodes
        .iter_enumerated()
        .map(|(node_id, node)| (node_id, node.scope_id()))
        .collect();
    drop(nodes);

    let mut normalizer = UndefinedNormalizer {
        ast: AstBuilder::new(source.allocator),
        scoping,
        node_scope_ids,
    };

    normalizer.visit_program(&mut source.program);

    Ok(())
}

struct UndefinedNormalizer<'a> {
    ast: AstBuilder<'a>,
    scoping: oxc_semantic::Scoping,
    node_scope_ids: HashMap<NodeId, ScopeId>,
}

impl<'a> VisitMut<'a> for UndefinedNormalizer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        if self.is_numeric_void_with_safe_undefined(expression) {
            let span = expression.span();
            *expression = self.ast.expression_identifier(span, "undefined");
            return;
        }

        walk_mut::walk_expression(self, expression);
    }
}

impl UndefinedNormalizer<'_> {
    fn is_numeric_void_with_safe_undefined(&self, expression: &Expression) -> bool {
        let Expression::UnaryExpression(unary) = expression else {
            return false;
        };

        if unary.operator != UnaryOperator::Void || !is_numeric_literal(&unary.argument) {
            return false;
        }

        let scope_id = self
            .node_scope_ids
            .get(&unary.node_id.get())
            .copied()
            .unwrap_or_else(|| self.scoping.root_scope_id());

        self.scoping
            .find_binding(scope_id, "undefined".into())
            .is_none()
    }
}

fn is_numeric_literal(expression: &Expression) -> bool {
    match expression {
        Expression::NumericLiteral(_) => true,
        Expression::ParenthesizedExpression(parenthesized) => {
            is_numeric_literal(&parenthesized.expression)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn transforms_void_zero_to_undefined() {
        define_ast_inline_test(transform_ast)(
            "
if (void 0 !== a) {
  console.log('a')
}
",
            "
if (undefined !== a) {
  console.log(\"a\");
}
",
        );
    }

    #[test]
    fn transforms_numeric_void_literals_to_undefined() {
        define_ast_inline_test(transform_ast)(
            "
void 0
void 99
void(0)
",
            "
undefined;
undefined;
undefined;
",
        );
    }

    #[test]
    fn leaves_void_function_calls_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
void function() {
  console.log('a')
  return void a()
}
",
            "
void function() {
  console.log(\"a\");
  return void a();
};
",
        );
    }

    #[test]
    fn leaves_void_numeric_when_undefined_is_declared_in_scope_chain() {
        define_ast_inline_test(transform_ast)(
            "
var undefined = 42;

console.log(void 0);

if (undefined !== a) {
  console.log('a', void 0);
}
",
            "
var undefined = 42;
console.log(void 0);
if (undefined !== a) {
  console.log(\"a\", void 0);
}
",
        );
    }

    #[test]
    fn transforms_outside_shadowing_scope_only() {
        define_ast_inline_test(transform_ast)(
            "
console.log(void 0);
function test(undefined) {
  return void 0;
}
console.log(void 1);
",
            "
console.log(undefined);
function test(undefined) {
  return void 0;
}
console.log(undefined);
",
        );
    }
}
