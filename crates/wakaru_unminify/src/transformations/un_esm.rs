use std::collections::HashSet;

use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{
        Argument, BindingPattern, CallExpression, Expression, ImportDeclaration,
        ImportDeclarationSpecifier, ImportOrExportKind, ModuleExportName, Program, PropertyKey,
        Statement, VariableDeclaration, VariableDeclarator,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::Span;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::{ParsedSourceFile, SyntheticTrailingComment};

use crate::transformations::runtime_helpers::babel::{
    interop_require_default, interop_require_wildcard,
};

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    interop_require_default::transform_ast(source)?;
    interop_require_wildcard::transform_ast(source)?;

    let ast = AstBuilder::new(source.allocator);
    let mut transformer = CommonJsImportTransformer::new(ast);
    transformer.transform_program(&mut source.program);

    let mut dynamic_import_transformer = DynamicRequireTransformer {
        ast: AstBuilder::new(source.allocator),
    };
    dynamic_import_transformer.visit_program(&mut source.program);

    let mut annotator = MissingRequireAnnotator {
        synthetic_trailing_comments: &mut source.synthetic_trailing_comments,
    };
    annotator.visit_program(&mut source.program);

    Ok(())
}

struct CommonJsImportTransformer<'a> {
    ast: AstBuilder<'a>,
    imports: ImportManager,
}

impl<'a> CommonJsImportTransformer<'a> {
    fn new(ast: AstBuilder<'a>) -> Self {
        Self {
            ast,
            imports: ImportManager::default(),
        }
    }

    fn transform_program(&mut self, program: &mut Program<'a>) {
        let old_body = program.body.take_in(self.ast);
        let mut kept_body = self.ast.vec_with_capacity(old_body.len());

        for statement in old_body {
            match statement {
                Statement::ImportDeclaration(import) => {
                    self.imports.collect_import(&import);
                }
                Statement::ExpressionStatement(statement)
                    if self.collect_bare_require(&statement.expression) => {}
                Statement::VariableDeclaration(declaration)
                    if self.collect_variable_require(&declaration) => {}
                statement => kept_body.push(statement),
            }
        }

        let import_count = self.imports.statement_count();
        let mut new_body = self
            .ast
            .vec_with_capacity(import_count.saturating_add(kept_body.len()));
        self.imports.push_import_statements(self.ast, &mut new_body);
        new_body.extend(kept_body);
        program.body = new_body;
    }

    fn collect_bare_require(&mut self, expression: &Expression<'a>) -> bool {
        let Some(source) = require_call_source(expression) else {
            return false;
        };

        self.imports.add_bare(source);
        true
    }

    fn collect_variable_require(&mut self, declaration: &VariableDeclaration<'a>) -> bool {
        if declaration.declarations.len() != 1 {
            return false;
        }

        let declarator = &declaration.declarations[0];
        if self.collect_basic_require(declarator) {
            return true;
        }

        self.collect_member_require(declarator)
    }

    fn collect_basic_require(&mut self, declarator: &VariableDeclarator<'a>) -> bool {
        let Some(init) = &declarator.init else {
            return false;
        };
        let Some(source) = require_call_source(init) else {
            return false;
        };

        match &declarator.id {
            BindingPattern::BindingIdentifier(identifier) => {
                self.imports.add_default(source, identifier.name.as_str());
                true
            }
            BindingPattern::ObjectPattern(_) => {
                let Some(imports) = named_imports_from_object_pattern(&declarator.id) else {
                    return false;
                };
                for (imported, local) in imports {
                    self.imports.add_named(source, &imported, &local);
                }
                true
            }
            _ => false,
        }
    }

    fn collect_member_require(&mut self, declarator: &VariableDeclarator<'a>) -> bool {
        let Some(Expression::StaticMemberExpression(member)) = &declarator.init else {
            return self.collect_computed_member_require(declarator);
        };
        let Some(source) = require_call_source(&member.object) else {
            return false;
        };

        self.collect_member_require_import(&declarator.id, source, member.property.name.as_str())
    }

    fn collect_computed_member_require(&mut self, declarator: &VariableDeclarator<'a>) -> bool {
        let Some(Expression::ComputedMemberExpression(member)) = &declarator.init else {
            return false;
        };
        let Some(source) = require_call_source(&member.object) else {
            return false;
        };
        let Expression::StringLiteral(property) = &member.expression else {
            return false;
        };

        self.collect_member_require_import(&declarator.id, source, property.value.as_str())
    }

    fn collect_member_require_import(
        &mut self,
        id: &BindingPattern<'a>,
        source: &str,
        imported: &str,
    ) -> bool {
        if imported != "default" && !is_valid_identifier_name(imported) {
            return false;
        }

        match id {
            BindingPattern::BindingIdentifier(identifier) if imported == "default" => {
                self.imports.add_default(source, identifier.name.as_str());
                true
            }
            BindingPattern::BindingIdentifier(identifier) => {
                self.imports
                    .add_named(source, imported, identifier.name.as_str());
                true
            }
            BindingPattern::ObjectPattern(_) if imported == "default" => {
                let Some(imports) = named_imports_from_object_pattern(id) else {
                    return false;
                };
                for (imported, local) in imports {
                    self.imports.add_named(source, &imported, &local);
                }
                true
            }
            _ => false,
        }
    }
}

struct DynamicRequireTransformer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for DynamicRequireTransformer<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        walk_mut::walk_expression(self, expression);

        let Some(source) = dynamic_require_source(expression) else {
            return;
        };

        *expression = self.ast.expression_import(
            Span::default(),
            self.ast
                .expression_string_literal(Span::default(), self.ast.str(&source), None),
            None,
            None,
        );
    }
}

#[derive(Default)]
struct ImportManager {
    order: Vec<String>,
    buckets: Vec<ImportBucket>,
}

#[derive(Default)]
struct ImportBucket {
    source: String,
    bare: bool,
    defaults: Vec<String>,
    namespaces: Vec<String>,
    named: Vec<NamedImport>,
    named_seen: HashSet<(String, String)>,
}

struct NamedImport {
    imported: String,
    local: String,
}

impl ImportManager {
    fn collect_import(&mut self, import: &ImportDeclaration) {
        let source = import.source.value.as_str();
        let Some(specifiers) = &import.specifiers else {
            self.add_bare(source);
            return;
        };

        for specifier in specifiers {
            match specifier {
                ImportDeclarationSpecifier::ImportDefaultSpecifier(default) => {
                    self.add_default(source, default.local.name.as_str());
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace) => {
                    self.add_namespace(source, namespace.local.name.as_str());
                }
                ImportDeclarationSpecifier::ImportSpecifier(named) => {
                    let Some(imported) = imported_name(&named.imported) else {
                        continue;
                    };
                    self.add_named(source, imported, named.local.name.as_str());
                }
            }
        }
    }

    fn add_bare(&mut self, source: &str) {
        self.bucket_mut(source).bare = true;
    }

    fn add_default(&mut self, source: &str, local: &str) {
        self.bucket_mut(source).defaults.push(local.to_string());
    }

    fn add_namespace(&mut self, source: &str, local: &str) {
        self.bucket_mut(source).namespaces.push(local.to_string());
    }

    fn add_named(&mut self, source: &str, imported: &str, local: &str) {
        let bucket = self.bucket_mut(source);
        let key = (imported.to_string(), local.to_string());
        if !bucket.named_seen.insert(key) {
            return;
        }

        bucket.named.push(NamedImport {
            imported: imported.to_string(),
            local: local.to_string(),
        });
    }

    fn statement_count(&self) -> usize {
        self.buckets
            .iter()
            .map(|bucket| {
                let has_named_or_default = !bucket.defaults.is_empty() || !bucket.named.is_empty();
                let combined = usize::from(has_named_or_default);
                let extra_defaults = bucket.defaults.len().saturating_sub(1);
                let bare = usize::from(bucket.bare && !has_named_or_default);
                bare + bucket.namespaces.len() + combined + extra_defaults
            })
            .sum()
    }

    fn push_import_statements<'a>(
        &self,
        ast: AstBuilder<'a>,
        statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        for source in &self.order {
            let Some(bucket) = self.buckets.iter().find(|bucket| &bucket.source == source) else {
                continue;
            };

            for namespace in &bucket.namespaces {
                statements.push(import_statement(
                    ast,
                    &bucket.source,
                    ast.vec_from_array([namespace_import_specifier(ast, namespace)]),
                ));
            }

            if bucket.defaults.is_empty() && bucket.named.is_empty() {
                if bucket.bare {
                    statements.push(bare_import_statement(ast, &bucket.source));
                }
                continue;
            }

            let mut specifiers = ast.vec();
            if let Some(default) = bucket.defaults.first() {
                specifiers.push(default_import_specifier(ast, default));
            }
            for named in &bucket.named {
                specifiers.push(named_import_specifier(ast, &named.imported, &named.local));
            }
            statements.push(import_statement(ast, &bucket.source, specifiers));

            for default in bucket.defaults.iter().skip(1) {
                statements.push(import_statement(
                    ast,
                    &bucket.source,
                    ast.vec_from_array([default_import_specifier(ast, default)]),
                ));
            }
        }
    }

    fn bucket_mut(&mut self, source: &str) -> &mut ImportBucket {
        if let Some(index) = self
            .buckets
            .iter()
            .position(|bucket| bucket.source == source)
        {
            return &mut self.buckets[index];
        }

        self.order.push(source.to_string());
        self.buckets.push(ImportBucket {
            source: source.to_string(),
            ..ImportBucket::default()
        });
        self.buckets.last_mut().expect("bucket was just pushed")
    }
}

struct MissingRequireAnnotator<'b> {
    synthetic_trailing_comments: &'b mut Vec<SyntheticTrailingComment>,
}

impl<'a> VisitMut<'a> for MissingRequireAnnotator<'_> {
    fn visit_call_expression(&mut self, call: &mut CallExpression<'a>) {
        if is_require_callee(&call.callee) {
            if let Some(Argument::NumericLiteral(literal)) = call.arguments.first() {
                let raw = literal
                    .raw
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| number_string(literal.value));
                self.synthetic_trailing_comments
                    .push(SyntheticTrailingComment {
                        candidates: vec![format!("require({raw})")],
                        replacement: format!("require({raw}/* wakaru:missing */)"),
                    });
            }
        }

        walk_mut::walk_call_expression(self, call);
    }
}

fn bare_import_statement<'a>(ast: AstBuilder<'a>, source: &str) -> Statement<'a> {
    Statement::ImportDeclaration(ast.alloc_import_declaration(
        Span::default(),
        None,
        ast.string_literal(Span::default(), ast.str(source), None),
        None,
        None::<oxc_allocator::Box<'a, oxc_ast::ast::WithClause<'a>>>,
        ImportOrExportKind::Value,
    ))
}

fn import_statement<'a>(
    ast: AstBuilder<'a>,
    source: &str,
    specifiers: oxc_allocator::Vec<'a, ImportDeclarationSpecifier<'a>>,
) -> Statement<'a> {
    Statement::ImportDeclaration(ast.alloc_import_declaration(
        Span::default(),
        Some(specifiers),
        ast.string_literal(Span::default(), ast.str(source), None),
        None,
        None::<oxc_allocator::Box<'a, oxc_ast::ast::WithClause<'a>>>,
        ImportOrExportKind::Value,
    ))
}

fn default_import_specifier<'a>(
    ast: AstBuilder<'a>,
    local: &str,
) -> ImportDeclarationSpecifier<'a> {
    ast.import_declaration_specifier_import_default_specifier(
        Span::default(),
        ast.binding_identifier(Span::default(), ast.ident(local)),
    )
}

fn namespace_import_specifier<'a>(
    ast: AstBuilder<'a>,
    local: &str,
) -> ImportDeclarationSpecifier<'a> {
    ast.import_declaration_specifier_import_namespace_specifier(
        Span::default(),
        ast.binding_identifier(Span::default(), ast.ident(local)),
    )
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

fn require_call_source<'a>(expression: &'a Expression<'a>) -> Option<&'a str> {
    let Expression::CallExpression(call) = expression else {
        return None;
    };
    if !is_require_callee(&call.callee) || call.arguments.len() != 1 {
        return None;
    }

    let Some(Argument::StringLiteral(source)) = call.arguments.first() else {
        return None;
    };
    Some(source.value.as_str())
}

fn dynamic_require_source(expression: &Expression) -> Option<String> {
    let Expression::CallExpression(call) = expression else {
        return None;
    };
    if !is_then_call(call) || call.arguments.len() != 1 {
        return None;
    }

    let Some(Argument::ArrowFunctionExpression(arrow)) = call.arguments.first() else {
        return None;
    };
    if !arrow.expression
        || !arrow.params.items.is_empty()
        || arrow.params.rest.is_some()
        || arrow.body.statements.len() != 1
    {
        return None;
    }

    let Some(Statement::ExpressionStatement(statement)) = arrow.body.statements.first() else {
        return None;
    };

    require_call_source(&statement.expression).map(str::to_string)
}

fn is_then_call(call: &CallExpression) -> bool {
    let Expression::StaticMemberExpression(then_member) = &call.callee else {
        return false;
    };
    if then_member.property.name != "then" {
        return false;
    }

    let Expression::CallExpression(resolve_call) = &then_member.object else {
        return false;
    };
    if !resolve_call.arguments.is_empty() {
        return false;
    }

    let Expression::StaticMemberExpression(resolve_member) = &resolve_call.callee else {
        return false;
    };
    resolve_member.property.name == "resolve"
        && matches!(&resolve_member.object, Expression::Identifier(identifier) if identifier.name == "Promise")
}

fn is_require_callee(expression: &Expression) -> bool {
    matches!(expression, Expression::Identifier(identifier) if identifier.name == "require")
}

fn named_imports_from_object_pattern(id: &BindingPattern) -> Option<Vec<(String, String)>> {
    let BindingPattern::ObjectPattern(pattern) = id else {
        return None;
    };

    let mut imports = Vec::with_capacity(pattern.properties.len());
    for property in &pattern.properties {
        let PropertyKey::StaticIdentifier(key) = &property.key else {
            return None;
        };
        let BindingPattern::BindingIdentifier(value) = &property.value else {
            return None;
        };

        imports.push((
            key.name.as_str().to_string(),
            value.name.as_str().to_string(),
        ));
    }

    Some(imports)
}

fn imported_name<'a>(imported: &'a ModuleExportName<'a>) -> Option<&'a str> {
    match imported {
        ModuleExportName::IdentifierName(identifier) => Some(identifier.name.as_str()),
        ModuleExportName::IdentifierReference(identifier) => Some(identifier.name.as_str()),
        ModuleExportName::StringLiteral(_) => None,
    }
}

fn is_valid_identifier_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    (first == '_' || first == '$' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphanumeric())
}

fn number_string(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn converts_top_level_requires_to_imports() {
        define_ast_inline_test(transform_ast)(
            "
var foo = require('foo');
var { bar, baz: qux } = require('foo');
var baz = require('baz').default;
var baz1 = require('baz2').baz3;
require('side-effect');
",
            "
import foo, { bar, baz as qux } from \"foo\";
import baz from \"baz\";
import { baz3 as baz1 } from \"baz2\";
import \"side-effect\";
",
        );
    }

    #[test]
    fn dedupes_existing_and_collected_imports() {
        define_ast_inline_test(transform_ast)(
            "
import 'foo';
import { bar } from 'foo';
require('foo');
var baz = require('foo').baz;
",
            "
import { bar, baz } from \"foo\";
",
        );
    }

    #[test]
    fn leaves_non_top_level_requires_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
function fn() {
  require('foo');
  var bar = require('bar');
  var baz = require('baz').baz;
  return bar + baz;
}
",
            "
function fn() {
  require(\"foo\");
  var bar = require(\"bar\");
  var baz = require(\"baz\").baz;
  return bar + baz;
}
",
        );
    }

    #[test]
    fn annotates_missing_numeric_require() {
        define_ast_inline_test(transform_ast)(
            "
var foo = require(9527);
",
            "
var foo = require(9527/* wakaru:missing */);
",
        );
    }

    #[test]
    fn runs_interop_default_before_collecting_imports() {
        define_ast_inline_test(transform_ast)(
            "
var _interopRequireDefault = require(\"@babel/runtime/helpers/interopRequireDefault\");
var _foo = _interopRequireDefault(require(\"foo\"));
_foo.default();
",
            "
import _foo from \"foo\";
_foo();
",
        );
    }

    #[test]
    fn converts_promise_then_requires_to_dynamic_imports() {
        define_ast_inline_test(transform_ast)(
            "
var _interopRequireWildcard = require(\"@babel/runtime/helpers/interopRequireWildcard\");
Promise.resolve().then(() => require('foo'));
Promise.resolve().then(() => _interopRequireWildcard(require('bar')));
",
            "
import(\"foo\");
import(\"bar\");
",
        );
    }
}
