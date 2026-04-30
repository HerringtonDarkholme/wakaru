use std::collections::{HashMap, HashSet};

use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{
        Argument, AssignmentTarget, BindingPattern, Expression, ImportDeclaration,
        ImportDeclarationSpecifier, Program, Statement, VariableDeclaration, VariableDeclarator,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk, walk_mut, Visit, VisitMut};
use oxc_semantic::SemanticBuilder;
use oxc_span::GetSpan;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

use crate::utils::is_helper_function_call::is_helper_callee;

const MODULE_NAME: &str = "@babel/runtime/helpers/interopRequireDefault";
const MODULE_ESM_NAME: &str = "@babel/runtime/helpers/esm/interopRequireDefault";

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let helper_locals = find_helper_locals(&source.program);
    if helper_locals.is_empty() {
        return Ok(());
    }

    let reference_counts = helper_reference_counts(&source.program, &helper_locals);
    let aliases = {
        let mut alias_collector = InteropRequireDefaultAliasCollector {
            helper_locals: &helper_locals,
            aliases: HashSet::new(),
        };
        alias_collector.visit_program(&source.program);
        alias_collector.aliases
    };

    let mut restorer = InteropRequireDefaultRestorer {
        ast: AstBuilder::new(source.allocator),
        helper_locals,
        aliases,
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

struct InteropRequireDefaultAliasCollector<'b> {
    helper_locals: &'b [String],
    aliases: HashSet<String>,
}

impl<'a> Visit<'a> for InteropRequireDefaultAliasCollector<'_> {
    fn visit_variable_declarator(&mut self, declarator: &VariableDeclarator<'a>) {
        if let BindingPattern::BindingIdentifier(identifier) = &declarator.id {
            if declarator
                .init
                .as_ref()
                .is_some_and(|init| self.is_interop_call_or_default_member(init))
            {
                self.aliases.insert(identifier.name.as_str().to_string());
            }
        }

        walk::walk_variable_declarator(self, declarator);
    }

    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if let Expression::AssignmentExpression(assignment) = expression {
            if let AssignmentTarget::AssignmentTargetIdentifier(identifier) = &assignment.left {
                if self.is_interop_call_or_default_member(&assignment.right) {
                    self.aliases.insert(identifier.name.as_str().to_string());
                }
            }
        }

        walk::walk_expression(self, expression);
    }
}

impl InteropRequireDefaultAliasCollector<'_> {
    fn is_interop_call_or_default_member(&self, expression: &Expression) -> bool {
        is_default_member_of_interop_call(expression, self.helper_locals)
            || is_interop_call(expression, self.helper_locals)
    }
}

struct InteropRequireDefaultRestorer<'a> {
    ast: AstBuilder<'a>,
    helper_locals: Vec<String>,
    aliases: HashSet<String>,
    processed_counts: HashMap<String, usize>,
}

impl<'a> VisitMut<'a> for InteropRequireDefaultRestorer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        if let Some(helper_local) = self.match_default_member_call(expression) {
            self.replace_default_member_call(expression, helper_local);
            return;
        }

        walk_mut::walk_expression(self, expression);

        if self.restore_alias_default_sequence(expression)
            || self.restore_alias_sequence(expression)
            || self.restore_alias_default(expression)
        {
            return;
        }

        if let Some(helper_local) = self.match_interop_call(expression) {
            self.replace_helper_call(expression, helper_local);
        }
    }
}

impl<'a> InteropRequireDefaultRestorer<'a> {
    fn match_interop_call(&self, expression: &Expression) -> Option<String> {
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

    fn match_default_member_call(&self, expression: &Expression) -> Option<String> {
        let Expression::StaticMemberExpression(member) = expression else {
            return None;
        };
        if member.property.name.as_str() != "default" {
            return None;
        }

        self.match_interop_call(&member.object)
    }

    fn replace_default_member_call(
        &mut self,
        expression: &mut Expression<'a>,
        helper_local: String,
    ) {
        let Expression::StaticMemberExpression(member) = expression.take_in(self.ast) else {
            return;
        };
        let Expression::CallExpression(call) = member.unbox().object else {
            return;
        };
        let Some(argument) = call
            .unbox()
            .arguments
            .into_iter()
            .next()
            .and_then(argument_to_expression)
        else {
            return;
        };

        *expression = argument;
        *self.processed_counts.entry(helper_local).or_default() += 1;
    }

    fn replace_helper_call(&mut self, expression: &mut Expression<'a>, helper_local: String) {
        let Expression::CallExpression(call) = expression.take_in(self.ast) else {
            return;
        };
        let Some(argument) = call
            .unbox()
            .arguments
            .into_iter()
            .next()
            .and_then(argument_to_expression)
        else {
            return;
        };

        *expression = argument;
        *self.processed_counts.entry(helper_local).or_default() += 1;
    }

    fn restore_alias_default_sequence(&mut self, expression: &mut Expression<'a>) -> bool {
        let Expression::SequenceExpression(sequence) = expression else {
            return false;
        };
        if sequence.expressions.len() != 2
            || !matches!(&sequence.expressions[0], Expression::NumericLiteral(number) if number.value == 0.0)
        {
            return false;
        }

        let Some(alias) = default_member_alias(&sequence.expressions[1], &self.aliases) else {
            return false;
        };
        let span = sequence.span();
        *expression = self
            .ast
            .expression_identifier(span, self.ast.allocator.alloc_str(&alias));
        true
    }

    fn restore_alias_sequence(&mut self, expression: &mut Expression<'a>) -> bool {
        let Expression::SequenceExpression(sequence) = expression else {
            return false;
        };
        if sequence.expressions.len() != 2
            || !matches!(&sequence.expressions[0], Expression::NumericLiteral(number) if number.value == 0.0)
        {
            return false;
        }

        let Expression::Identifier(identifier) = &sequence.expressions[1] else {
            return false;
        };
        if !self.aliases.contains(identifier.name.as_str()) {
            return false;
        }

        let span = sequence.span();
        let alias = identifier.name.as_str().to_string();
        *expression = self
            .ast
            .expression_identifier(span, self.ast.allocator.alloc_str(&alias));
        true
    }

    fn restore_alias_default(&mut self, expression: &mut Expression<'a>) -> bool {
        let Some(alias) = default_member_alias(expression, &self.aliases) else {
            return false;
        };
        let span = expression.span();
        *expression = self
            .ast
            .expression_identifier(span, self.ast.allocator.alloc_str(&alias));
        true
    }
}

fn is_interop_call(expression: &Expression, helper_locals: &[String]) -> bool {
    let Expression::CallExpression(call) = expression else {
        return false;
    };
    if call.arguments.len() != 1
        || matches!(call.arguments.first(), Some(Argument::SpreadElement(_)))
    {
        return false;
    }

    helper_locals
        .iter()
        .any(|helper| is_helper_callee(&call.callee, helper))
}

fn is_default_member_of_interop_call(expression: &Expression, helper_locals: &[String]) -> bool {
    let Expression::StaticMemberExpression(member) = expression else {
        return false;
    };

    member.property.name.as_str() == "default" && is_interop_call(&member.object, helper_locals)
}

fn default_member_alias(expression: &Expression, aliases: &HashSet<String>) -> Option<String> {
    match expression {
        Expression::StaticMemberExpression(member)
            if member.property.name.as_str() == "default"
                && matches!(&member.object, Expression::Identifier(identifier) if aliases.contains(identifier.name.as_str())) =>
        {
            let Expression::Identifier(identifier) = &member.object else {
                unreachable!();
            };
            Some(identifier.name.as_str().to_string())
        }
        Expression::ComputedMemberExpression(member)
            if matches!(&member.object, Expression::Identifier(identifier) if aliases.contains(identifier.name.as_str()))
                && matches!(&member.expression, Expression::StringLiteral(property) if property.value.as_str() == "default") =>
        {
            let Expression::Identifier(identifier) = &member.object else {
                unreachable!();
            };
            Some(identifier.name.as_str().to_string())
        }
        _ => None,
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
    match expression {
        Expression::CallExpression(call)
            if matches!(&call.callee, Expression::Identifier(identifier) if identifier.name.as_str() == "require")
                && call.arguments.len() == 1 =>
        {
            let Some(Argument::StringLiteral(source)) = call.arguments.first() else {
                return None;
            };
            Some(source.value.as_str())
        }
        Expression::StaticMemberExpression(member)
            if member.property.name.as_str() == "default" =>
        {
            require_source(&member.object)
        }
        _ => None,
    }
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
    fn restores_interop_require_default_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
import _source$es6Default from "source";

var _interopRequireDefault = require("@babel/runtime/helpers/interopRequireDefault");

_interopRequireDefault(_a);
_b = _interopRequireDefault(require("b"));
var _c = _interopRequireDefault(require("c"));
var _d = _interopRequireDefault(require("d")).default;

var _source = _interopRequireDefault(_source$es6Default).default;
_source;
var _source2 = _interopRequireDefault(_source$es6Default);
_source2.default;
_source2["default"];

(0, _b.default)();
(0, _c.default)();
"#,
            r#"
import _source$es6Default from "source";
_a;
_b = require("b");
var _c = require("c");
var _d = require("d");
var _source = _source$es6Default;
_source;
var _source2 = _source$es6Default;
_source2;
_source2;
_b();
_c();
"#,
        );
    }

    #[test]
    fn restores_helper_imported_through_require_default() {
        define_ast_inline_test(transform_ast)(
            r#"
var _interopRequireDefault = require("@babel/runtime/helpers/interopRequireDefault").default;
var _interopRequireDefault2 = _interopRequireDefault(require("@babel/runtime/helpers/interopRequireDefault"));
console.log(_interopRequireDefault2.default);
"#,
            r#"
var _interopRequireDefault2 = require("@babel/runtime/helpers/interopRequireDefault");
console.log(_interopRequireDefault2);
"#,
        );
    }
}
