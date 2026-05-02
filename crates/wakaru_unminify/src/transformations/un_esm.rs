use std::collections::{HashMap, HashSet};

use oxc_allocator::TakeIn;
use oxc_ast::{
    ast::{
        Argument, AssignmentTarget, BindingPattern, CallExpression, Declaration,
        ExportDefaultDeclarationKind, ExportSpecifier, Expression, ImportDeclaration,
        ImportDeclarationSpecifier, ImportOrExportKind, ModuleExportName, Program, PropertyKey,
        Statement, VariableDeclaration, VariableDeclarationKind, VariableDeclarator, WithClause,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::Span;
use oxc_syntax::operator::AssignmentOperator;
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

    let mut export_transformer = CommonJsExportTransformer {
        ast: AstBuilder::new(source.allocator),
    };
    export_transformer.transform_program(&mut source.program);

    let mut annotator = MissingRequireAnnotator {
        synthetic_trailing_comments: &mut source.synthetic_trailing_comments,
    };
    annotator.visit_program(&mut source.program);

    Ok(())
}

struct CommonJsExportTransformer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> CommonJsExportTransformer<'a> {
    fn transform_program(&mut self, program: &mut Program<'a>) {
        let old_body = program.body.take_in(self.ast);
        let last_exports = collect_last_export_indices(&old_body);
        let mut declared_names = collect_top_level_declared_names(&old_body);
        let mut new_body = self.ast.vec_with_capacity(old_body.len());

        for (index, statement) in old_body.into_iter().enumerate() {
            let replacements =
                self.transform_statement(index, statement, &last_exports, &mut declared_names);
            new_body.extend(replacements);
        }

        program.body = new_body;
    }

    fn transform_statement(
        &mut self,
        index: usize,
        statement: Statement<'a>,
        last_exports: &HashMap<String, usize>,
        declared_names: &mut HashSet<String>,
    ) -> oxc_allocator::Vec<'a, Statement<'a>> {
        let statement = match self.transform_expression_export(
            index,
            statement,
            last_exports,
            declared_names,
        ) {
            Ok(replacements) => return replacements,
            Err(statement) => statement,
        };

        let statement =
            match self.transform_variable_export(index, statement, last_exports, declared_names) {
                Ok(replacements) => return replacements,
                Err(statement) => statement,
            };

        let mut statements = self.ast.vec_with_capacity(1);
        statements.push(statement);
        statements
    }

    fn transform_expression_export(
        &mut self,
        index: usize,
        statement: Statement<'a>,
        last_exports: &HashMap<String, usize>,
        declared_names: &mut HashSet<String>,
    ) -> std::result::Result<oxc_allocator::Vec<'a, Statement<'a>>, Statement<'a>> {
        let Statement::ExpressionStatement(mut statement) = statement else {
            return Err(statement);
        };
        let expression = statement.expression.take_in(self.ast);
        let mut assignment = match expression {
            Expression::AssignmentExpression(assignment) => assignment,
            expression => return Err(self.ast.statement_expression(statement.span, expression)),
        };
        if assignment.operator != AssignmentOperator::Assign {
            return Err(self.ast.statement_expression(
                statement.span,
                Expression::AssignmentExpression(assignment),
            ));
        }

        let Some(export_name) = export_assignment_name(&assignment.left) else {
            return Err(self.ast.statement_expression(
                statement.span,
                Expression::AssignmentExpression(assignment),
            ));
        };
        if last_exports.get(&export_name).copied() != Some(index) {
            return Ok(self.ast.vec());
        }

        let mut statements = self.ast.vec();
        let right = assignment.right.take_in(self.ast);
        if export_name == "default" {
            statements.push(self.export_default_statement(statement.span, right));
        } else {
            self.push_named_export_assignment(
                statement.span,
                &export_name,
                right,
                VariableDeclarationKind::Const,
                declared_names,
                &mut statements,
            );
        }
        Ok(statements)
    }

    fn transform_variable_export(
        &mut self,
        index: usize,
        statement: Statement<'a>,
        last_exports: &HashMap<String, usize>,
        _declared_names: &mut HashSet<String>,
    ) -> std::result::Result<oxc_allocator::Vec<'a, Statement<'a>>, Statement<'a>> {
        let Statement::VariableDeclaration(mut declaration) = statement else {
            return Err(statement);
        };
        if declaration.declarations.len() != 1 {
            return Err(Statement::VariableDeclaration(declaration));
        }

        let span = declaration.span;
        let kind = declaration.kind;
        let declare = declaration.declare;
        let mut declarations = declaration.declarations.take_in(self.ast);
        let mut declarator = declarations
            .pop()
            .expect("single-declaration vector should have one item");
        let Some(id_name) = binding_identifier_name(&declarator.id).map(str::to_string) else {
            declaration.declarations = self.ast.vec_from_array([declarator]);
            return Err(Statement::VariableDeclaration(declaration));
        };
        let mut assignment = match declarator.init.take() {
            Some(Expression::AssignmentExpression(assignment)) => assignment,
            init => {
                declarator.init = init;
                declaration.declarations = self.ast.vec_from_array([declarator]);
                return Err(Statement::VariableDeclaration(declaration));
            }
        };
        if assignment.operator != AssignmentOperator::Assign {
            declarator.init = Some(Expression::AssignmentExpression(assignment));
            declaration.declarations = self.ast.vec_from_array([declarator]);
            return Err(Statement::VariableDeclaration(declaration));
        }

        let Some(export_name) = export_assignment_name(&assignment.left) else {
            declarator.init = Some(Expression::AssignmentExpression(assignment));
            declaration.declarations = self.ast.vec_from_array([declarator]);
            return Err(Statement::VariableDeclaration(declaration));
        };
        if last_exports.get(&export_name).copied() != Some(index) {
            return Ok(self.ast.vec());
        }

        let mut statements = self.ast.vec();
        let right = assignment.right.take_in(self.ast);
        if export_name == "default" {
            declarator.init = Some(right);
            statements.push(self.variable_declaration_statement(span, kind, declare, declarator));
            statements
                .push(self.export_default_statement(span, self.identifier_expression(&id_name)));
        } else if id_name == export_name {
            declarator.init = Some(right);
            statements.push(self.export_variable_statement(span, kind, declare, declarator));
        } else {
            statements.push(self.variable_declaration_statement(
                span,
                kind,
                declare,
                self.variable_declarator(
                    span,
                    kind,
                    declarator.id,
                    self.identifier_expression(&export_name),
                ),
            ));
            statements.push(self.export_variable_statement(
                span,
                kind,
                declare,
                self.variable_declarator(
                    span,
                    kind,
                    self.binding_identifier_pattern(&export_name),
                    right,
                ),
            ));
        }
        Ok(statements)
    }

    fn push_named_export_assignment(
        &mut self,
        span: Span,
        export_name: &str,
        right: Expression<'a>,
        kind: VariableDeclarationKind,
        declared_names: &mut HashSet<String>,
        statements: &mut oxc_allocator::Vec<'a, Statement<'a>>,
    ) {
        if let Some(local) = expression_identifier_name(&right) {
            if local == export_name {
                statements.push(self.export_specifier_statement(span, local, export_name));
                return;
            }

            if declared_names.contains(export_name) {
                statements.push(self.export_specifier_statement(span, local, export_name));
                return;
            }
        }

        if declared_names.contains(export_name) {
            let local = generate_name(export_name, declared_names);
            declared_names.insert(local.clone());
            statements.push(self.variable_declaration_statement(
                span,
                kind,
                false,
                self.variable_declarator(
                    span,
                    kind,
                    self.binding_identifier_pattern(&local),
                    right,
                ),
            ));
            statements.push(self.export_specifier_statement(span, &local, export_name));
            return;
        }

        declared_names.insert(export_name.to_string());
        statements.push(self.export_variable_statement(
            span,
            kind,
            false,
            self.variable_declarator(
                span,
                kind,
                self.binding_identifier_pattern(export_name),
                right,
            ),
        ));
    }

    fn export_default_statement(&self, span: Span, expression: Expression<'a>) -> Statement<'a> {
        Statement::ExportDefaultDeclaration(self.ast.alloc_export_default_declaration(
            span,
            expression_into_export_default_kind(expression),
        ))
    }

    fn export_variable_statement(
        &self,
        span: Span,
        kind: VariableDeclarationKind,
        declare: bool,
        declarator: VariableDeclarator<'a>,
    ) -> Statement<'a> {
        let declaration = Declaration::VariableDeclaration(self.ast.alloc_variable_declaration(
            span,
            kind,
            self.ast.vec_from_array([declarator]),
            declare,
        ));
        Statement::ExportNamedDeclaration(self.ast.alloc_export_named_declaration(
            span,
            Some(declaration),
            self.ast.vec(),
            None,
            ImportOrExportKind::Value,
            None::<oxc_allocator::Box<'a, WithClause<'a>>>,
        ))
    }

    fn export_specifier_statement(&self, span: Span, local: &str, exported: &str) -> Statement<'a> {
        Statement::ExportNamedDeclaration(
            self.ast.alloc_export_named_declaration(
                span,
                None,
                self.ast
                    .vec_from_array([self.export_specifier(span, local, exported)]),
                None,
                ImportOrExportKind::Value,
                None::<oxc_allocator::Box<'a, WithClause<'a>>>,
            ),
        )
    }

    fn export_specifier(&self, span: Span, local: &str, exported: &str) -> ExportSpecifier<'a> {
        self.ast.export_specifier(
            span,
            self.module_export_name(local),
            self.module_export_name(exported),
            ImportOrExportKind::Value,
        )
    }

    fn variable_declaration_statement(
        &self,
        span: Span,
        kind: VariableDeclarationKind,
        declare: bool,
        declarator: VariableDeclarator<'a>,
    ) -> Statement<'a> {
        Statement::VariableDeclaration(self.ast.alloc_variable_declaration(
            span,
            kind,
            self.ast.vec_from_array([declarator]),
            declare,
        ))
    }

    fn variable_declarator(
        &self,
        span: Span,
        kind: VariableDeclarationKind,
        id: BindingPattern<'a>,
        init: Expression<'a>,
    ) -> VariableDeclarator<'a> {
        self.ast.variable_declarator(
            span,
            kind,
            id,
            None::<oxc_allocator::Box<'a, oxc_ast::ast::TSTypeAnnotation<'a>>>,
            Some(init),
            false,
        )
    }

    fn binding_identifier_pattern(&self, name: &str) -> BindingPattern<'a> {
        self.ast
            .binding_pattern_binding_identifier(Span::default(), self.ast.ident(name))
    }

    fn identifier_expression(&self, name: &str) -> Expression<'a> {
        self.ast
            .expression_identifier(Span::default(), self.ast.ident(name))
    }

    fn module_export_name(&self, name: &str) -> ModuleExportName<'a> {
        self.ast
            .module_export_name_identifier_name(Span::default(), self.ast.ident(name))
    }
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

fn collect_last_export_indices(body: &oxc_allocator::Vec<Statement>) -> HashMap<String, usize> {
    let mut exports = HashMap::new();

    for (index, statement) in body.iter().enumerate() {
        let Some(name) = statement_export_name(statement) else {
            continue;
        };
        exports.insert(name, index);
    }

    exports
}

fn statement_export_name(statement: &Statement) -> Option<String> {
    match statement {
        Statement::ExpressionStatement(statement) => {
            let Expression::AssignmentExpression(assignment) = &statement.expression else {
                return None;
            };
            if assignment.operator != AssignmentOperator::Assign {
                return None;
            }
            if export_assignment_name(&assignment.left).as_deref() == Some("default")
                && is_module_exports_expression(&assignment.right)
            {
                return None;
            }
            export_assignment_name(&assignment.left)
        }
        Statement::VariableDeclaration(declaration) => {
            if declaration.declarations.len() != 1 {
                return None;
            }

            let declarator = &declaration.declarations[0];
            let Some(Expression::AssignmentExpression(assignment)) = &declarator.init else {
                return None;
            };
            if assignment.operator != AssignmentOperator::Assign {
                return None;
            }

            export_assignment_name(&assignment.left)
        }
        _ => None,
    }
}

fn export_assignment_name(target: &AssignmentTarget) -> Option<String> {
    match target {
        AssignmentTarget::StaticMemberExpression(member) => {
            if is_module_exports_root(&member.object, member.property.name.as_str()) {
                return Some("default".to_string());
            }

            if is_export_object_expression(&member.object) {
                return Some(member.property.name.as_str().to_string());
            }

            None
        }
        AssignmentTarget::ComputedMemberExpression(member) => {
            if !is_export_object_expression(&member.object) {
                return None;
            }

            let Expression::StringLiteral(property) = &member.expression else {
                return None;
            };

            Some(property.value.as_str().to_string())
        }
        _ => None,
    }
}

fn is_export_object_expression(expression: &Expression) -> bool {
    matches!(expression, Expression::Identifier(identifier) if identifier.name == "exports")
        || is_module_exports_expression(expression)
}

fn is_module_exports_expression(expression: &Expression) -> bool {
    let Expression::StaticMemberExpression(member) = expression else {
        return false;
    };
    is_module_exports_root(&member.object, member.property.name.as_str())
}

fn is_module_exports_root(object: &Expression, property: &str) -> bool {
    property == "exports"
        && matches!(object, Expression::Identifier(identifier) if identifier.name == "module")
}

fn collect_top_level_declared_names(body: &oxc_allocator::Vec<Statement>) -> HashSet<String> {
    let mut names = HashSet::new();

    for statement in body {
        match statement {
            Statement::ImportDeclaration(import) => {
                if let Some(specifiers) = &import.specifiers {
                    for specifier in specifiers {
                        match specifier {
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(default) => {
                                names.insert(default.local.name.as_str().to_string());
                            }
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace) => {
                                names.insert(namespace.local.name.as_str().to_string());
                            }
                            ImportDeclarationSpecifier::ImportSpecifier(named) => {
                                names.insert(named.local.name.as_str().to_string());
                            }
                        }
                    }
                }
            }
            Statement::VariableDeclaration(declaration) => {
                for declarator in &declaration.declarations {
                    if let Some(name) = binding_identifier_name(&declarator.id) {
                        names.insert(name.to_string());
                    }
                }
            }
            Statement::FunctionDeclaration(function) => {
                if let Some(id) = &function.id {
                    names.insert(id.name.as_str().to_string());
                }
            }
            Statement::ClassDeclaration(class) => {
                if let Some(id) = &class.id {
                    names.insert(id.name.as_str().to_string());
                }
            }
            _ => {}
        }
    }

    names
}

fn binding_identifier_name<'b, 'a>(pattern: &'b BindingPattern<'a>) -> Option<&'b str> {
    let BindingPattern::BindingIdentifier(identifier) = pattern else {
        return None;
    };
    Some(identifier.name.as_str())
}

fn expression_identifier_name<'b, 'a>(expression: &'b Expression<'a>) -> Option<&'b str> {
    let Expression::Identifier(identifier) = expression else {
        return None;
    };
    Some(identifier.name.as_str())
}

fn generate_name(base: &str, declared_names: &HashSet<String>) -> String {
    let mut index = 1;
    loop {
        let candidate = format!("{base}_{index}");
        if !declared_names.contains(&candidate) {
            return candidate;
        }
        index += 1;
    }
}

fn expression_into_export_default_kind(expression: Expression) -> ExportDefaultDeclarationKind {
    match expression {
        Expression::BooleanLiteral(value) => ExportDefaultDeclarationKind::BooleanLiteral(value),
        Expression::NullLiteral(value) => ExportDefaultDeclarationKind::NullLiteral(value),
        Expression::NumericLiteral(value) => ExportDefaultDeclarationKind::NumericLiteral(value),
        Expression::BigIntLiteral(value) => ExportDefaultDeclarationKind::BigIntLiteral(value),
        Expression::RegExpLiteral(value) => ExportDefaultDeclarationKind::RegExpLiteral(value),
        Expression::StringLiteral(value) => ExportDefaultDeclarationKind::StringLiteral(value),
        Expression::TemplateLiteral(value) => ExportDefaultDeclarationKind::TemplateLiteral(value),
        Expression::Identifier(value) => ExportDefaultDeclarationKind::Identifier(value),
        Expression::MetaProperty(value) => ExportDefaultDeclarationKind::MetaProperty(value),
        Expression::Super(value) => ExportDefaultDeclarationKind::Super(value),
        Expression::ArrayExpression(value) => ExportDefaultDeclarationKind::ArrayExpression(value),
        Expression::ArrowFunctionExpression(value) => {
            ExportDefaultDeclarationKind::ArrowFunctionExpression(value)
        }
        Expression::AssignmentExpression(value) => {
            ExportDefaultDeclarationKind::AssignmentExpression(value)
        }
        Expression::AwaitExpression(value) => ExportDefaultDeclarationKind::AwaitExpression(value),
        Expression::BinaryExpression(value) => {
            ExportDefaultDeclarationKind::BinaryExpression(value)
        }
        Expression::CallExpression(value) => ExportDefaultDeclarationKind::CallExpression(value),
        Expression::ChainExpression(value) => ExportDefaultDeclarationKind::ChainExpression(value),
        Expression::ClassExpression(value) => ExportDefaultDeclarationKind::ClassExpression(value),
        Expression::ComputedMemberExpression(value) => {
            ExportDefaultDeclarationKind::ComputedMemberExpression(value)
        }
        Expression::ConditionalExpression(value) => {
            ExportDefaultDeclarationKind::ConditionalExpression(value)
        }
        Expression::FunctionExpression(value) => {
            ExportDefaultDeclarationKind::FunctionExpression(value)
        }
        Expression::ImportExpression(value) => {
            ExportDefaultDeclarationKind::ImportExpression(value)
        }
        Expression::LogicalExpression(value) => {
            ExportDefaultDeclarationKind::LogicalExpression(value)
        }
        Expression::NewExpression(value) => ExportDefaultDeclarationKind::NewExpression(value),
        Expression::ObjectExpression(value) => {
            ExportDefaultDeclarationKind::ObjectExpression(value)
        }
        Expression::ParenthesizedExpression(value) => {
            ExportDefaultDeclarationKind::ParenthesizedExpression(value)
        }
        Expression::PrivateFieldExpression(value) => {
            ExportDefaultDeclarationKind::PrivateFieldExpression(value)
        }
        Expression::StaticMemberExpression(value) => {
            ExportDefaultDeclarationKind::StaticMemberExpression(value)
        }
        Expression::SequenceExpression(value) => {
            ExportDefaultDeclarationKind::SequenceExpression(value)
        }
        Expression::TaggedTemplateExpression(value) => {
            ExportDefaultDeclarationKind::TaggedTemplateExpression(value)
        }
        Expression::ThisExpression(value) => ExportDefaultDeclarationKind::ThisExpression(value),
        Expression::UnaryExpression(value) => ExportDefaultDeclarationKind::UnaryExpression(value),
        Expression::UpdateExpression(value) => {
            ExportDefaultDeclarationKind::UpdateExpression(value)
        }
        Expression::YieldExpression(value) => ExportDefaultDeclarationKind::YieldExpression(value),
        Expression::PrivateInExpression(value) => {
            ExportDefaultDeclarationKind::PrivateInExpression(value)
        }
        Expression::JSXElement(value) => ExportDefaultDeclarationKind::JSXElement(value),
        Expression::JSXFragment(value) => ExportDefaultDeclarationKind::JSXFragment(value),
        Expression::TSAsExpression(value) => ExportDefaultDeclarationKind::TSAsExpression(value),
        Expression::TSSatisfiesExpression(value) => {
            ExportDefaultDeclarationKind::TSSatisfiesExpression(value)
        }
        Expression::TSTypeAssertion(value) => ExportDefaultDeclarationKind::TSTypeAssertion(value),
        Expression::TSNonNullExpression(value) => {
            ExportDefaultDeclarationKind::TSNonNullExpression(value)
        }
        Expression::TSInstantiationExpression(value) => {
            ExportDefaultDeclarationKind::TSInstantiationExpression(value)
        }
        Expression::V8IntrinsicExpression(value) => {
            ExportDefaultDeclarationKind::V8IntrinsicExpression(value)
        }
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

    #[test]
    fn converts_commonjs_default_and_named_exports() {
        define_ast_inline_test(transform_ast)(
            "
module.exports = { foo: 1 };
exports.bar = bar;
module.exports.baz = 2;
",
            "
export default { foo: 1 };
export { bar };
export const baz = 2;
",
        );
    }

    #[test]
    fn keeps_last_duplicate_exports_and_skips_default_self_alias() {
        define_ast_inline_test(transform_ast)(
            "
module.exports.foo = void 0;
module.exports.foo = 2;
function foo() {}
module.exports = foo;
module.exports.default = module.exports;
",
            "
const foo_1 = 2;
export { foo_1 as foo };
function foo() {}
export default foo;
",
        );
    }

    #[test]
    fn converts_babel_assignment_variable_exports() {
        define_ast_inline_test(transform_ast)(
            "
var foo = exports.foo = 1;
var bar = exports.baz = 2;
var qux = module.exports.default = 3;
",
            "
export var foo = 1;
var bar = baz;
export var baz = 2;
var qux = 3;
export default qux;
",
        );
    }

    #[test]
    fn resolves_named_export_binding_conflicts() {
        define_ast_inline_test(transform_ast)(
            "
var foo = 1;
console.log(foo);
exports.foo = 2;

const baz = 3;
const qux = 4;
module.exports.baz = qux;
",
            "
var foo = 1;
console.log(foo);
const foo_1 = 2;
export { foo_1 as foo };
const baz = 3;
const qux = 4;
export { qux as baz };
",
        );
    }
}
