use oxc_allocator::Box;
use oxc_ast::{
    ast::{Function, IdentifierReference},
    AstBuilder,
};
use oxc_ast_visit::{walk, walk_mut, Visit, VisitMut};
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_span::Span;
use oxc_syntax::scope::ScopeFlags;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();

    let mut transformer = ParameterRestTransformer {
        ast: AstBuilder::new(source.allocator),
        scoping,
    };

    transformer.visit_program(&mut source.program);

    Ok(())
}

struct ParameterRestTransformer<'a> {
    ast: AstBuilder<'a>,
    scoping: Scoping,
}

impl<'a> VisitMut<'a> for ParameterRestTransformer<'a> {
    fn visit_function(&mut self, function: &mut Function<'a>, flags: ScopeFlags) {
        walk_mut::walk_function(self, function, flags);
        self.try_transform_function(function);
    }
}

impl<'a> ParameterRestTransformer<'a> {
    fn try_transform_function(&self, function: &mut Function<'a>) {
        if !function.params.items.is_empty() || function.params.rest.is_some() {
            return;
        }

        let Some(body) = function.body.as_mut() else {
            return;
        };
        let Some(scope_id) = function.scope_id.get() else {
            return;
        };

        if self.scoping.find_binding(scope_id, "args".into()).is_some()
            || self
                .scoping
                .find_binding(scope_id, "arguments".into())
                .is_some()
        {
            return;
        }

        let mut scanner = ArgumentsReferenceScanner {
            scoping: &self.scoping,
            found: false,
            has_args_conflict: false,
        };
        scanner.visit_function_body(body);

        if !scanner.found || scanner.has_args_conflict {
            return;
        }

        let mut renamer = ArgumentsRenamer { ast: self.ast };
        renamer.visit_function_body(body);

        function.params.rest =
            Some(self.ast.alloc_formal_parameter_rest(
                Span::default(),
                self.ast.vec(),
                self.ast.binding_rest_element(
                    Span::default(),
                    self.ast.binding_pattern_binding_identifier(
                        Span::default(),
                        self.ast.ident("args"),
                    ),
                ),
                None::<Box<'a, oxc_ast::ast::TSTypeAnnotation<'a>>>,
            ));
    }
}

struct ArgumentsReferenceScanner<'s> {
    scoping: &'s Scoping,
    found: bool,
    has_args_conflict: bool,
}

impl<'a> Visit<'a> for ArgumentsReferenceScanner<'_> {
    fn visit_function(&mut self, _function: &Function<'a>, _flags: ScopeFlags) {}

    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        if identifier.name != "arguments" {
            walk::walk_identifier_reference(self, identifier);
            return;
        }

        self.found = true;

        let Some(reference_id) = identifier.reference_id.get() else {
            self.has_args_conflict = true;
            return;
        };
        let reference_scope_id = self.scoping.get_reference(reference_id).scope_id();
        if self
            .scoping
            .find_binding(reference_scope_id, "args".into())
            .is_some()
        {
            self.has_args_conflict = true;
        }
    }
}

struct ArgumentsRenamer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for ArgumentsRenamer<'a> {
    fn visit_function(&mut self, _function: &mut Function<'a>, _flags: ScopeFlags) {}

    fn visit_identifier_reference(&mut self, identifier: &mut IdentifierReference<'a>) {
        if identifier.name == "arguments" {
            identifier.name = self.ast.ident("args");
            return;
        }

        walk_mut::walk_identifier_reference(self, identifier);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn replaces_arguments_in_function_declaration() {
        define_ast_inline_test(transform_ast)(
            "
function foo() {
  console.log(arguments);
}
",
            "
function foo(...args) {
  console.log(args);
}
",
        );
    }

    #[test]
    fn replaces_arguments_in_function_expression_and_nested_arrow() {
        define_ast_inline_test(transform_ast)(
            "
var foo = function() {
  var bar = () => console.log(arguments);
}
",
            "
var foo = function(...args) {
  var bar = () => console.log(args);
};
",
        );
    }

    #[test]
    fn leaves_arrow_function_and_existing_params_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
var foo = () => console.log(arguments);
function bar(a) {
  console.log(arguments);
}
",
            "
var foo = () => console.log(arguments);
function bar(a) {
  console.log(arguments);
}
",
        );
    }

    #[test]
    fn skips_args_conflicts() {
        define_ast_inline_test(transform_ast)(
            "
var args = [];
function foo() {
  console.log(args, arguments);
}
function bar() {
  if (true) {
    const args = 0;
    console.log(arguments);
  }
}
",
            "
var args = [];
function foo() {
  console.log(args, arguments);
}
function bar() {
  if (true) {
    const args = 0;
    console.log(arguments);
  }
}
",
        );
    }
}
