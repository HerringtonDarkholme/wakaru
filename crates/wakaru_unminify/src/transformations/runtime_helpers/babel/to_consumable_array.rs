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
use oxc_span::GetSpan;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

use crate::utils::is_helper_function_call::is_helper_callee;

const MODULE_NAME: &str = "@babel/runtime/helpers/toConsumableArray";
const MODULE_ESM_NAME: &str = "@babel/runtime/helpers/esm/toConsumableArray";

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let helper_locals = find_helper_locals(&source.program);
    if helper_locals.is_empty() {
        return Ok(());
    }

    let reference_counts = helper_reference_counts(&source.program, &helper_locals);
    let mut restorer = ToConsumableArrayRestorer {
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
            AstBuilder::new(source.allocator),
        );
    }

    Ok(())
}

struct ToConsumableArrayRestorer<'a> {
    ast: AstBuilder<'a>,
    helper_locals: Vec<String>,
    processed_counts: HashMap<String, usize>,
}

impl<'a> VisitMut<'a> for ToConsumableArrayRestorer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        walk_mut::walk_expression(self, expression);

        let Some(helper_local) = self.match_to_consumable_array_call(expression) else {
            return;
        };

        let span = expression.span();
        let Expression::CallExpression(call) = expression.take_in(self.ast) else {
            return;
        };
        let call = call.unbox();
        let Some(argument) = call
            .arguments
            .into_iter()
            .next()
            .and_then(argument_to_expression)
        else {
            return;
        };

        let mut elements = self.ast.vec_with_capacity(1);
        elements.push(
            self.ast
                .array_expression_element_spread_element(argument.span(), argument),
        );

        *expression = self.ast.expression_array(span, elements);
        *self.processed_counts.entry(helper_local).or_default() += 1;
    }
}

impl ToConsumableArrayRestorer<'_> {
    fn match_to_consumable_array_call(&self, expression: &Expression) -> Option<String> {
        let Expression::CallExpression(call) = expression else {
            return None;
        };

        if call.arguments.len() != 1
            || matches!(call.arguments.first(), Some(Argument::SpreadElement(_)))
        {
            return None;
        }

        self.helper_locals
            .iter()
            .find(|helper| is_helper_callee(&call.callee, helper))
            .cloned()
    }
}

fn find_helper_locals(program: &Program) -> Vec<String> {
    let mut locals = Vec::new();

    for statement in &program.body {
        match statement {
            Statement::ImportDeclaration(import)
                if is_helper_source(import.source.value.as_str()) =>
            {
                collect_import_locals(import, &mut locals);
            }
            Statement::VariableDeclaration(declaration) => {
                collect_require_locals(declaration, &mut locals);
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

fn collect_require_locals(declaration: &VariableDeclaration, locals: &mut Vec<String>) {
    for declarator in &declaration.declarations {
        if !is_helper_require_declarator(declarator) {
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
    ast: AstBuilder<'a>,
) {
    let old_body = program.body.take_in(ast);
    let mut new_body = ast.vec_with_capacity(old_body.len());

    for statement in old_body {
        match statement {
            Statement::ImportDeclaration(import)
                if is_helper_source(import.source.value.as_str()) =>
            {
                if let Some(statement) = remove_import_helpers(import, removable_helpers) {
                    new_body.push(statement);
                }
            }
            Statement::VariableDeclaration(declaration) => {
                if let Some(statement) = remove_require_helpers(declaration, removable_helpers, ast)
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
    ast: AstBuilder<'a>,
) -> Option<Statement<'a>> {
    let old_declarations = declaration.declarations.take_in(ast);
    let mut kept_declarations = ast.vec();

    for declarator in old_declarations {
        if require_declarator_is_removable(&declarator, removable_helpers) {
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
) -> bool {
    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return false;
    };

    removable_helpers.contains(identifier.name.as_str()) && is_helper_require_declarator(declarator)
}

fn is_helper_require_declarator(declarator: &VariableDeclarator) -> bool {
    let Some(init) = &declarator.init else {
        return false;
    };

    require_source(init).is_some_and(is_helper_source)
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

fn is_helper_source(source: &str) -> bool {
    matches!(source, MODULE_NAME | MODULE_ESM_NAME)
}

fn argument_to_expression(argument: Argument) -> Option<Expression> {
    macro_rules! expression_variant {
        ($variant:ident, $value:ident) => {
            Some(Expression::$variant($value))
        };
    }

    match argument {
        Argument::SpreadElement(_) => None,
        Argument::BooleanLiteral(value) => expression_variant!(BooleanLiteral, value),
        Argument::NullLiteral(value) => expression_variant!(NullLiteral, value),
        Argument::NumericLiteral(value) => expression_variant!(NumericLiteral, value),
        Argument::BigIntLiteral(value) => expression_variant!(BigIntLiteral, value),
        Argument::RegExpLiteral(value) => expression_variant!(RegExpLiteral, value),
        Argument::StringLiteral(value) => expression_variant!(StringLiteral, value),
        Argument::TemplateLiteral(value) => expression_variant!(TemplateLiteral, value),
        Argument::Identifier(value) => expression_variant!(Identifier, value),
        Argument::MetaProperty(value) => expression_variant!(MetaProperty, value),
        Argument::Super(value) => expression_variant!(Super, value),
        Argument::ArrayExpression(value) => expression_variant!(ArrayExpression, value),
        Argument::ArrowFunctionExpression(value) => {
            expression_variant!(ArrowFunctionExpression, value)
        }
        Argument::AssignmentExpression(value) => expression_variant!(AssignmentExpression, value),
        Argument::AwaitExpression(value) => expression_variant!(AwaitExpression, value),
        Argument::BinaryExpression(value) => expression_variant!(BinaryExpression, value),
        Argument::CallExpression(value) => expression_variant!(CallExpression, value),
        Argument::ChainExpression(value) => expression_variant!(ChainExpression, value),
        Argument::ClassExpression(value) => expression_variant!(ClassExpression, value),
        Argument::ConditionalExpression(value) => expression_variant!(ConditionalExpression, value),
        Argument::FunctionExpression(value) => expression_variant!(FunctionExpression, value),
        Argument::ImportExpression(value) => expression_variant!(ImportExpression, value),
        Argument::LogicalExpression(value) => expression_variant!(LogicalExpression, value),
        Argument::NewExpression(value) => expression_variant!(NewExpression, value),
        Argument::ObjectExpression(value) => expression_variant!(ObjectExpression, value),
        Argument::ParenthesizedExpression(value) => {
            expression_variant!(ParenthesizedExpression, value)
        }
        Argument::SequenceExpression(value) => expression_variant!(SequenceExpression, value),
        Argument::TaggedTemplateExpression(value) => {
            expression_variant!(TaggedTemplateExpression, value)
        }
        Argument::ThisExpression(value) => expression_variant!(ThisExpression, value),
        Argument::UnaryExpression(value) => expression_variant!(UnaryExpression, value),
        Argument::UpdateExpression(value) => expression_variant!(UpdateExpression, value),
        Argument::YieldExpression(value) => expression_variant!(YieldExpression, value),
        Argument::PrivateInExpression(value) => expression_variant!(PrivateInExpression, value),
        Argument::JSXElement(value) => expression_variant!(JSXElement, value),
        Argument::JSXFragment(value) => expression_variant!(JSXFragment, value),
        Argument::TSAsExpression(value) => expression_variant!(TSAsExpression, value),
        Argument::TSSatisfiesExpression(value) => {
            expression_variant!(TSSatisfiesExpression, value)
        }
        Argument::TSTypeAssertion(value) => expression_variant!(TSTypeAssertion, value),
        Argument::TSNonNullExpression(value) => expression_variant!(TSNonNullExpression, value),
        Argument::TSInstantiationExpression(value) => {
            expression_variant!(TSInstantiationExpression, value)
        }
        Argument::ComputedMemberExpression(value) => {
            expression_variant!(ComputedMemberExpression, value)
        }
        Argument::StaticMemberExpression(value) => {
            expression_variant!(StaticMemberExpression, value)
        }
        Argument::PrivateFieldExpression(value) => {
            expression_variant!(PrivateFieldExpression, value)
        }
        Argument::V8IntrinsicExpression(value) => expression_variant!(V8IntrinsicExpression, value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn restores_cjs_to_consumable_array_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
var _toConsumableArray = require("@babel/runtime/helpers/toConsumableArray");

_toConsumableArray(a);
_toConsumableArray.default(a);
(0, _toConsumableArray)(a);
(0, _toConsumableArray.default)(a);
"#,
            "
[...a];
[...a];
[...a];
[...a];
",
        );
    }

    #[test]
    fn restores_esm_to_consumable_array_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
import _toConsumableArray from "@babel/runtime/helpers/esm/toConsumableArray";

_toConsumableArray(a);
_toConsumableArray.default(a);
(0, _toConsumableArray)(a);
(0, _toConsumableArray.default)(a);
"#,
            "
[...a];
[...a];
[...a];
[...a];
",
        );
    }

    #[test]
    fn leaves_invalid_calls_unchanged() {
        define_ast_inline_test(transform_ast)(
            r#"
var _toConsumableArray = require("@babel/runtime/helpers/toConsumableArray");

_toConsumableArray(a, b);
_toConsumableArray.default(a, b);
(0, _toConsumableArray)(a, b);
(0, _toConsumableArray.default)(a, b);
"#,
            r#"
var _toConsumableArray = require("@babel/runtime/helpers/toConsumableArray");
_toConsumableArray(a, b);
_toConsumableArray.default(a, b);
(0, _toConsumableArray)(a, b);
(0, _toConsumableArray.default)(a, b);
"#,
        );
    }

    #[test]
    fn keeps_helper_declaration_when_unprocessed_references_remain() {
        define_ast_inline_test(transform_ast)(
            r#"
var _toConsumableArray = require("@babel/runtime/helpers/toConsumableArray");

_toConsumableArray(a);
console.log(_toConsumableArray);
"#,
            r#"
var _toConsumableArray = require("@babel/runtime/helpers/toConsumableArray");
[...a];
console.log(_toConsumableArray);
"#,
        );
    }
}
