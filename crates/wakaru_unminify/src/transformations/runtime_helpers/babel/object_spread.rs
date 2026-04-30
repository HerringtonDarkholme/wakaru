use std::collections::{HashMap, HashSet};

use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{
        Argument, BindingPattern, Expression, ImportDeclaration, ImportDeclarationSpecifier,
        Program, Statement, VariableDeclaration, VariableDeclarator,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_semantic::SemanticBuilder;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

use super::_spread;
use crate::utils::is_helper_function_call::is_helper_callee;

const MODULE_NAME: &str = "@babel/runtime/helpers/objectSpread2";
const MODULE_ESM_NAME: &str = "@babel/runtime/helpers/esm/objectSpread2";
const FALLBACK_MODULE_NAME: &str = "@babel/runtime/helpers/objectSpread";
const FALLBACK_MODULE_ESM_NAME: &str = "@babel/runtime/helpers/esm/objectSpread";
const CHECKER_MODULE_NAME: &str = "@babel/runtime/helpers/objectDestructuringEmpty";
const CHECKER_MODULE_ESM_NAME: &str = "@babel/runtime/helpers/esm/objectDestructuringEmpty";

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    handle_object_destructuring_empty(source)?;
    _spread::transform_ast(
        source,
        &[
            MODULE_NAME,
            MODULE_ESM_NAME,
            FALLBACK_MODULE_NAME,
            FALLBACK_MODULE_ESM_NAME,
        ],
    )
}

fn handle_object_destructuring_empty(source: &mut ParsedSourceFile) -> Result<()> {
    let helper_sources = [CHECKER_MODULE_NAME, CHECKER_MODULE_ESM_NAME];
    let helper_locals = find_helper_locals(&source.program, &helper_sources);
    if helper_locals.is_empty() {
        return Ok(());
    }

    let reference_counts = helper_reference_counts(&source.program, &helper_locals);
    let mut restorer = ObjectDestructuringEmptyRestorer {
        ast: AstBuilder::new(source.allocator),
        helper_locals,
        processed_counts: HashMap::new(),
    };

    restorer.visit_program(&mut source.program);

    let removable_helpers = restorer
        .processed_counts
        .iter()
        .filter_map(|(helper, processed)| {
            (reference_counts.get(helper).copied().unwrap_or_default() == *processed)
                .then(|| helper.clone())
        })
        .collect::<HashSet<_>>();

    if !removable_helpers.is_empty() {
        remove_helper_declarations(
            &mut source.program,
            &removable_helpers,
            &helper_sources,
            AstBuilder::new(source.allocator),
        );
    }

    Ok(())
}

struct ObjectDestructuringEmptyRestorer<'a> {
    ast: AstBuilder<'a>,
    helper_locals: Vec<String>,
    processed_counts: HashMap<String, usize>,
}

impl<'a> VisitMut<'a> for ObjectDestructuringEmptyRestorer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        walk_mut::walk_expression(self, expression);

        let Some(helper_local) = self.match_object_destructuring_empty_sequence(expression) else {
            return;
        };

        let Expression::SequenceExpression(sequence) = expression.take_in(self.ast) else {
            return;
        };
        let mut expressions = sequence.unbox().expressions.into_iter();
        let _checker_call = expressions.next();
        let Some(replacement) = expressions.next() else {
            return;
        };

        *expression = replacement;
        *self.processed_counts.entry(helper_local).or_default() += 1;
    }
}

impl ObjectDestructuringEmptyRestorer<'_> {
    fn match_object_destructuring_empty_sequence(&self, expression: &Expression) -> Option<String> {
        let Expression::SequenceExpression(sequence) = expression else {
            return None;
        };

        if sequence.expressions.len() != 2 {
            return None;
        }

        let Expression::CallExpression(call) = &sequence.expressions[0] else {
            return None;
        };
        if call.arguments.len() != 1 {
            return None;
        }

        let Some(Argument::Identifier(argument)) = call.arguments.first() else {
            return None;
        };
        let Expression::Identifier(second) = &sequence.expressions[1] else {
            return None;
        };
        if argument.name.as_str() != second.name.as_str() {
            return None;
        }

        self.helper_locals
            .iter()
            .find(|helper| is_helper_callee(&call.callee, helper))
            .cloned()
    }
}

fn find_helper_locals(program: &Program, helper_sources: &[&str]) -> Vec<String> {
    let mut locals = Vec::new();

    for statement in &program.body {
        match statement {
            Statement::ImportDeclaration(import)
                if is_helper_source(import.source.value.as_str(), helper_sources) =>
            {
                collect_import_locals(import, &mut locals);
            }
            Statement::VariableDeclaration(declaration) => {
                collect_require_locals(declaration, helper_sources, &mut locals);
            }
            _ => {}
        }
    }

    locals
}

fn collect_import_locals(import: &ImportDeclaration, locals: &mut Vec<String>) {
    let Some(specifiers) = &import.specifiers else {
        return;
    };

    for specifier in specifiers {
        match specifier {
            ImportDeclarationSpecifier::ImportDefaultSpecifier(default) => {
                locals.push(default.local.name.as_str().to_string());
            }
            ImportDeclarationSpecifier::ImportSpecifier(named) => {
                locals.push(named.local.name.as_str().to_string());
            }
            ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace) => {
                locals.push(namespace.local.name.as_str().to_string());
            }
        }
    }
}

fn collect_require_locals(
    declaration: &VariableDeclaration,
    helper_sources: &[&str],
    locals: &mut Vec<String>,
) {
    for declarator in &declaration.declarations {
        if !is_helper_require_declarator(declarator, helper_sources) {
            continue;
        }

        let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            continue;
        };

        locals.push(identifier.name.as_str().to_string());
    }
}

fn helper_reference_counts(program: &Program, helper_locals: &[String]) -> HashMap<String, usize> {
    let semantic = SemanticBuilder::new().build(program).semantic;
    let scoping = semantic.scoping();
    helper_locals
        .iter()
        .filter_map(|helper| {
            scoping
                .get_root_binding(helper.as_str().into())
                .map(|symbol_id| {
                    (
                        helper.clone(),
                        scoping.get_resolved_reference_ids(symbol_id).len(),
                    )
                })
        })
        .collect()
}

fn remove_helper_declarations<'a>(
    program: &mut Program<'a>,
    removable_helpers: &HashSet<String>,
    helper_sources: &[&str],
    ast: AstBuilder<'a>,
) {
    let old_body = program.body.take_in(ast);
    let mut new_body = ast.vec_with_capacity(old_body.len());

    for statement in old_body {
        match statement {
            Statement::ImportDeclaration(import)
                if is_helper_source(import.source.value.as_str(), helper_sources) =>
            {
                if let Some(statement) = remove_import_helpers(import, removable_helpers) {
                    new_body.push(statement);
                }
            }
            Statement::VariableDeclaration(declaration) => {
                if let Some(statement) =
                    remove_require_helpers(declaration, removable_helpers, helper_sources, ast)
                {
                    new_body.push(statement);
                }
            }
            statement => new_body.push(statement),
        }
    }

    program.body = new_body;
}

fn remove_import_helpers<'a>(
    mut import: oxc_allocator::Box<'a, ImportDeclaration<'a>>,
    removable_helpers: &HashSet<String>,
) -> Option<Statement<'a>> {
    let Some(specifiers) = &mut import.specifiers else {
        return Some(Statement::ImportDeclaration(import));
    };

    specifiers.retain(|specifier| !import_specifier_is_removable(specifier, removable_helpers));

    if specifiers.is_empty() {
        None
    } else {
        Some(Statement::ImportDeclaration(import))
    }
}

fn remove_require_helpers<'a>(
    mut declaration: oxc_allocator::Box<'a, VariableDeclaration<'a>>,
    removable_helpers: &HashSet<String>,
    helper_sources: &[&str],
    ast: AstBuilder<'a>,
) -> Option<Statement<'a>> {
    let old_declarations = declaration.declarations.take_in(ast);
    let mut kept_declarations = ast.vec();

    for declarator in old_declarations {
        if require_declarator_is_removable(&declarator, removable_helpers, helper_sources) {
            continue;
        }

        kept_declarations.push(declarator);
    }

    if kept_declarations.is_empty() {
        None
    } else {
        declaration.declarations = kept_declarations;
        Some(Statement::VariableDeclaration(declaration))
    }
}

fn import_specifier_is_removable(
    specifier: &ImportDeclarationSpecifier,
    removable_helpers: &HashSet<String>,
) -> bool {
    match specifier {
        ImportDeclarationSpecifier::ImportDefaultSpecifier(default) => {
            removable_helpers.contains(default.local.name.as_str())
        }
        ImportDeclarationSpecifier::ImportSpecifier(named) => {
            removable_helpers.contains(named.local.name.as_str())
        }
        ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace) => {
            removable_helpers.contains(namespace.local.name.as_str())
        }
    }
}

fn require_declarator_is_removable(
    declarator: &VariableDeclarator,
    removable_helpers: &HashSet<String>,
    helper_sources: &[&str],
) -> bool {
    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return false;
    };

    removable_helpers.contains(identifier.name.as_str())
        && is_helper_require_declarator(declarator, helper_sources)
}

fn is_helper_require_declarator(declarator: &VariableDeclarator, helper_sources: &[&str]) -> bool {
    let Some(init) = &declarator.init else {
        return false;
    };

    require_source(init).is_some_and(|source| is_helper_source(source, helper_sources))
}

fn require_source<'a>(expression: &'a Expression<'a>) -> Option<&'a str> {
    let Expression::CallExpression(call) = expression else {
        return None;
    };

    if !matches!(&call.callee, Expression::Identifier(identifier) if identifier.name.as_str() == "require")
        || call.arguments.len() != 1
    {
        return None;
    }

    let Some(Argument::StringLiteral(source)) = call.arguments.first() else {
        return None;
    };

    Some(source.value.as_str())
}

fn is_helper_source(source: &str, helper_sources: &[&str]) -> bool {
    helper_sources.contains(&source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn restores_cjs_object_spread_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
var _objectSpread2 = require("@babel/runtime/helpers/objectSpread2");

a = _objectSpread2({}, y);
b = _objectSpread2.default({}, y);
c = (0, _objectSpread2)({}, y);
d = (0, _objectSpread2.default)({}, y);
"#,
            "
a = { ...y };
b = { ...y };
c = { ...y };
d = { ...y };
",
        );
    }

    #[test]
    fn restores_esm_object_spread_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
import _objectSpread2 from "@babel/runtime/helpers/esm/objectSpread2";

a = _objectSpread2({}, y);
b = _objectSpread2.default({}, y);
c = (0, _objectSpread2)({}, y);
d = (0, _objectSpread2.default)({}, y);
"#,
            "
a = { ...y };
b = { ...y };
c = { ...y };
d = { ...y };
",
        );
    }

    #[test]
    fn restores_fallback_object_spread_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
import _objectSpread2 from "@babel/runtime/helpers/esm/objectSpread";

a = _objectSpread2({}, y);
b = _objectSpread2({ x }, y);
c = _objectSpread2({ x: z }, { y: "bar" });
d = _objectSpread2({ x }, { y: _objectSpread2({}, z) });
"#,
            r#"
a = { ...y };
b = {
  x,
  ...y
};
c = {
  x: z,
  y: "bar"
};
d = {
  x,
  y: { ...z }
};
"#,
        );
    }

    #[test]
    fn removes_object_destructuring_empty_checker_sequences() {
        define_ast_inline_test(transform_ast)(
            r#"
var _objectSpread2 = require("@babel/runtime/helpers/objectSpread2");
var _objectDestructuringEmpty = require("@babel/runtime/helpers/objectDestructuringEmpty");

a = _objectSpread2({}, (_objectDestructuringEmpty(y), y));
"#,
            "
a = { ...y };
",
        );
    }

    #[test]
    fn leaves_mismatched_checker_sequences_unchanged() {
        define_ast_inline_test(transform_ast)(
            r#"
var _objectDestructuringEmpty = require("@babel/runtime/helpers/objectDestructuringEmpty");

a = (_objectDestructuringEmpty(y), z);
"#,
            r#"
var _objectDestructuringEmpty = require("@babel/runtime/helpers/objectDestructuringEmpty");
a = (_objectDestructuringEmpty(y), z);
"#,
        );
    }
}
