use std::collections::{HashMap, HashSet};

use oxc_allocator::{CloneIn, TakeIn};
use oxc_ast::{
    ast::{
        Argument, ArrayExpressionElement, AssignmentExpression, AssignmentTarget,
        BindingIdentifier, BindingPattern, CallExpression, Expression, IdentifierReference,
        JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElementName,
        JSXExpression, JSXMemberExpressionObject, ObjectPropertyKind, PropertyKey, Statement,
        TemplateLiteral, VariableDeclarationKind, VariableDeclarator,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk, walk_mut, Visit, VisitMut};
use oxc_semantic::{ScopeId, Scoping, SemanticBuilder, SymbolId};
use oxc_span::{GetSpan, Span};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::{ParsedSourceFile, TransformationParams};

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();
    let runtime_config = JsxRuntimeConfig::from_params(source.params);

    let mut rename_collector = ComponentRenameCollector {
        runtime_config: runtime_config.clone(),
        scoping: &scoping,
        component_symbols: HashSet::new(),
        pending_display_names: Vec::new(),
        generated_names: HashMap::new(),
        renames: HashMap::new(),
    };
    rename_collector.visit_program(&mut source.program);
    rename_collector.finish_display_name_renames();

    let mut renamer = SymbolRenamer {
        ast: AstBuilder::new(source.allocator),
        scoping: &scoping,
        renames: rename_collector.renames,
    };
    renamer.visit_program(&mut source.program);

    let mut transformer = JsxTransformer {
        ast: AstBuilder::new(source.allocator),
        runtime_config,
        dynamic_component_names: Vec::new(),
        pending_dynamic_components: Vec::new(),
        string_tag_declarations: HashMap::new(),
        used_string_tag_declarations: HashSet::new(),
    };

    transformer.visit_program(&mut source.program);

    Ok(())
}

struct JsxTransformer<'a> {
    ast: AstBuilder<'a>,
    runtime_config: JsxRuntimeConfig,
    dynamic_component_names: Vec<HashSet<String>>,
    pending_dynamic_components: Vec<(Span, String, Expression<'a>)>,
    string_tag_declarations: HashMap<String, String>,
    used_string_tag_declarations: HashSet<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Runtime {
    Classic,
    Automatic,
}

const DEFAULT_PRAGMA_CANDIDATES: &[&str] = &[
    "createElement",
    "jsx",
    "jsxs",
    "_jsx",
    "_jsxs",
    "jsxDEV",
    "jsxsDEV",
];

const DEFAULT_PRAGMA_FRAG_CANDIDATES: &[&str] = &["Fragment"];

#[derive(Clone, Debug)]
struct JsxRuntimeConfig {
    pragmas: Vec<String>,
    pragma_frags: Vec<String>,
}

impl JsxRuntimeConfig {
    fn from_params(params: &TransformationParams) -> Self {
        Self {
            pragmas: pragma_candidates(params.un_jsx_pragma.as_deref(), DEFAULT_PRAGMA_CANDIDATES),
            pragma_frags: pragma_candidates(
                params.un_jsx_pragma_frag.as_deref(),
                DEFAULT_PRAGMA_FRAG_CANDIDATES,
            ),
        }
    }

    fn runtime(&self, callee: &Expression) -> Option<Runtime> {
        match without_parentheses(callee) {
            Expression::Identifier(identifier) => self.runtime_for_name(identifier.name.as_str()),
            Expression::StaticMemberExpression(member) => {
                let Expression::Identifier(object) = &member.object else {
                    return None;
                };
                if object.name == "document" {
                    return None;
                }
                self.runtime_for_name(member.property.name.as_str())
            }
            _ => None,
        }
    }

    fn is_fragment_tag(&self, tag: &JSXElementName) -> bool {
        let name = match tag {
            JSXElementName::Identifier(identifier) => Some(identifier.name.as_str()),
            JSXElementName::IdentifierReference(identifier) => Some(identifier.name.as_str()),
            JSXElementName::MemberExpression(member) => Some(member.property.name.as_str()),
            _ => None,
        };

        name.is_some_and(|name| self.pragma_frags.iter().any(|candidate| candidate == name))
    }

    fn runtime_for_name(&self, name: &str) -> Option<Runtime> {
        if !self.pragmas.iter().any(|candidate| candidate == name) {
            return None;
        }

        Some(if automatic_runtime_name(name) {
            Runtime::Automatic
        } else {
            Runtime::Classic
        })
    }
}

fn pragma_candidates(value: Option<&str>, defaults: &[&str]) -> Vec<String> {
    match value {
        Some(value) => vec![pragma_property(value).to_string()],
        None => defaults.iter().map(|value| (*value).to_string()).collect(),
    }
}

fn pragma_property(value: &str) -> &str {
    value.rsplit('.').next().unwrap_or(value)
}

fn automatic_runtime_name(name: &str) -> bool {
    matches!(
        name,
        "jsx" | "jsxs" | "_jsx" | "_jsxs" | "jsxDEV" | "jsxsDEV"
    )
}

struct ComponentRenameCollector<'s> {
    runtime_config: JsxRuntimeConfig,
    scoping: &'s Scoping,
    component_symbols: HashSet<SymbolId>,
    pending_display_names: Vec<(SymbolId, ScopeId, String)>,
    generated_names: HashMap<ScopeId, HashSet<String>>,
    renames: HashMap<SymbolId, String>,
}

impl<'a> VisitMut<'a> for ComponentRenameCollector<'_> {
    fn visit_variable_declarator(&mut self, declarator: &mut VariableDeclarator<'a>) {
        self.collect_component_declarator(declarator);
        walk_mut::walk_variable_declarator(self, declarator);
    }

    fn visit_assignment_expression(&mut self, assignment: &mut AssignmentExpression<'a>) {
        self.collect_display_name_assignment(assignment);
        walk_mut::walk_assignment_expression(self, assignment);
    }

    fn visit_call_expression(&mut self, call: &mut CallExpression<'a>) {
        self.rename_lowercase_component(call);
        walk_mut::walk_call_expression(self, call);
    }
}

impl ComponentRenameCollector<'_> {
    fn collect_component_declarator(&mut self, declarator: &VariableDeclarator) {
        let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return;
        };
        let Some(init) = &declarator.init else {
            return;
        };
        if !expression_contains_jsx_runtime_call(init, &self.runtime_config) {
            return;
        }
        let Some(symbol_id) = identifier.symbol_id.get() else {
            return;
        };
        self.component_symbols.insert(symbol_id);
    }

    fn collect_display_name_assignment(&mut self, assignment: &AssignmentExpression) {
        let AssignmentTarget::StaticMemberExpression(member) = &assignment.left else {
            return;
        };
        if member.property.name != "displayName" {
            return;
        }
        let Expression::Identifier(object) = &member.object else {
            return;
        };
        if object.name.len() > 2 {
            return;
        }
        let Expression::StringLiteral(display_name) = &assignment.right else {
            return;
        };

        let Some(symbol_id) = object
            .reference_id
            .get()
            .and_then(|reference_id| self.scoping.get_reference(reference_id).symbol_id())
        else {
            return;
        };
        let scope_id = self.scoping.symbol_scope_id(symbol_id);
        self.pending_display_names.push((
            symbol_id,
            scope_id,
            component_name(display_name.value.as_str()),
        ));
    }

    fn finish_display_name_renames(&mut self) {
        for (symbol_id, scope_id, base_name) in self.pending_display_names.clone() {
            if !self.component_symbols.contains(&symbol_id) {
                continue;
            }
            let new_name = self.generate_target_name(scope_id, &base_name);
            self.record_symbol_rename(symbol_id, scope_id, &new_name);
        }
    }

    fn rename_lowercase_component(&mut self, call: &CallExpression) {
        if self.runtime_config.runtime(&call.callee).is_none() {
            return;
        }

        let Some(Argument::Identifier(identifier)) = call.arguments.first() else {
            return;
        };
        if !identifier
            .name
            .as_str()
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase())
        {
            return;
        }

        let Some(symbol_id) = identifier
            .reference_id
            .get()
            .and_then(|reference_id| self.scoping.get_reference(reference_id).symbol_id())
        else {
            return;
        };
        if self.renames.contains_key(&symbol_id) {
            return;
        }

        let scope_id = self.scoping.symbol_scope_id(symbol_id);
        let new_name = self.generate_target_name(scope_id, &pascal_case(identifier.name.as_str()));
        self.record_symbol_rename(symbol_id, scope_id, &new_name);
    }

    fn record_symbol_rename(&mut self, symbol_id: SymbolId, scope_id: ScopeId, new_name: &str) {
        self.generated_names
            .entry(scope_id)
            .or_default()
            .insert(new_name.to_string());
        self.renames.insert(symbol_id, new_name.to_string());
    }

    fn generate_target_name(&self, scope_id: ScopeId, base: &str) -> String {
        if !self.is_name_occupied(scope_id, base) {
            return base.to_string();
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

    fn is_name_occupied(&self, mut scope_id: ScopeId, name: &str) -> bool {
        if self.scoping.find_binding(scope_id, name.into()).is_some() {
            return true;
        }

        loop {
            if self
                .generated_names
                .get(&scope_id)
                .is_some_and(|names| names.contains(name))
            {
                return true;
            }

            let Some(parent_scope_id) = self.scoping.scope_parent_id(scope_id) else {
                return false;
            };
            scope_id = parent_scope_id;
        }
    }
}

struct SymbolRenamer<'a, 's> {
    ast: AstBuilder<'a>,
    scoping: &'s Scoping,
    renames: HashMap<SymbolId, String>,
}

impl<'a> VisitMut<'a> for SymbolRenamer<'a, '_> {
    fn visit_binding_identifier(&mut self, identifier: &mut BindingIdentifier<'a>) {
        if let Some(new_name) = identifier
            .symbol_id
            .get()
            .and_then(|symbol_id| self.renames.get(&symbol_id))
        {
            identifier.name = self.ast.ident(new_name);
        }
    }

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

struct JsxRuntimeCallFinder {
    runtime_config: JsxRuntimeConfig,
    found: bool,
}

impl<'a> Visit<'a> for JsxRuntimeCallFinder {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if self.found {
            return;
        }
        if self.runtime_config.runtime(&call.callee).is_some() {
            self.found = true;
            return;
        }
        walk::walk_call_expression(self, call);
    }
}

fn expression_contains_jsx_runtime_call(
    expression: &Expression,
    runtime_config: &JsxRuntimeConfig,
) -> bool {
    let mut finder = JsxRuntimeCallFinder {
        runtime_config: runtime_config.clone(),
        found: false,
    };
    finder.visit_expression(expression);
    finder.found
}

impl<'a> VisitMut<'a> for JsxTransformer<'a> {
    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        let old_statements = statements.take_in(self.ast);
        let mut new_statements = self.ast.vec_with_capacity(old_statements.len());
        let previous_string_tags = self.string_tag_declarations.clone();
        let mut dynamic_names = HashSet::new();

        for statement in &old_statements {
            if let Some((name, value)) = string_tag_declaration(statement) {
                self.string_tag_declarations
                    .insert(name.to_string(), value.to_string());
            }
            collect_statement_binding_names(statement, &mut dynamic_names);
        }

        self.dynamic_component_names.push(dynamic_names);

        for mut statement in old_statements {
            let pending_start = self.pending_dynamic_components.len();
            walk_mut::walk_statement(self, &mut statement);
            let pending = self
                .pending_dynamic_components
                .split_off(pending_start)
                .into_iter()
                .map(|(span, name, expression)| {
                    self.dynamic_component_declaration(span, name, expression)
                });
            new_statements.extend(pending);
            new_statements.push(statement);
        }

        let mut final_statements = self.ast.vec_with_capacity(new_statements.len());
        for statement in new_statements {
            if removable_used_string_tag_declaration(&statement, &self.used_string_tag_declarations)
            {
                continue;
            }
            final_statements.push(statement);
        }

        self.dynamic_component_names.pop();
        self.string_tag_declarations = previous_string_tags;
        *statements = final_statements;
    }

    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        walk_mut::walk_expression(self, expression);

        if let Some(jsx) = self.to_jsx_expression(expression) {
            *expression = jsx;
        }
    }
}

impl<'a> JsxTransformer<'a> {
    fn to_jsx_expression(&mut self, expression: &Expression<'a>) -> Option<Expression<'a>> {
        let Expression::CallExpression(call) = without_parentheses(expression) else {
            return None;
        };
        let runtime = self.runtime_config.runtime(&call.callee)?;
        if call.arguments.len() < 2 {
            return None;
        }

        let tag_expression =
            argument_to_expression(call.arguments[0].clone_in(self.ast.allocator))?;
        let tag = self
            .to_jsx_tag(tag_expression.clone_in(self.ast.allocator))
            .or_else(|| self.dynamic_component_tag(call.span, tag_expression))?;
        if capitalization_invalid(&tag) {
            return None;
        }

        let mut attributes = self.to_jsx_attributes(&call.arguments[1])?;
        if runtime == Runtime::Automatic {
            self.prepend_key_attribute(&mut attributes, call.arguments.get(2));
        }
        let mut children = self.ast.vec();
        if runtime == Runtime::Automatic {
            if let Some(attribute_children) = self.take_children_attribute(&mut attributes) {
                children = attribute_children;
            }
        } else {
            for child in call.arguments.iter().skip(2) {
                if let Some(child) = self.to_jsx_child(child) {
                    children.push(child);
                }
            }
        }

        let span = call.span;
        if attributes.is_empty() && self.runtime_config.is_fragment_tag(&tag) {
            return Some(Expression::JSXFragment(self.ast.alloc_jsx_fragment(
                span,
                self.ast.jsx_opening_fragment(span),
                children,
                self.ast.jsx_closing_fragment(span),
            )));
        }

        let closing_element = if children.is_empty() {
            None
        } else {
            Some(
                self.ast
                    .jsx_closing_element(span, tag.clone_in(self.ast.allocator)),
            )
        };
        let opening_element =
            self.ast
                .jsx_opening_element(span, tag, None::<oxc_allocator::Box<'a, _>>, attributes);

        Some(Expression::JSXElement(self.ast.alloc_jsx_element(
            span,
            opening_element,
            children,
            closing_element,
        )))
    }

    fn to_jsx_tag(&mut self, expression: Expression<'a>) -> Option<JSXElementName<'a>> {
        match expression {
            Expression::StringLiteral(literal) => {
                Some(self.jsx_tag_from_static_string(literal.span, literal.value.as_str()))
            }
            Expression::TemplateLiteral(literal) => {
                let tag_name = static_template_literal_value(&literal)?;
                Some(self.jsx_tag_from_static_string(literal.span, tag_name))
            }
            Expression::Identifier(identifier) => {
                if let Some(tag_name) = self
                    .string_tag_declarations
                    .get(identifier.name.as_str())
                    .cloned()
                {
                    self.used_string_tag_declarations
                        .insert(identifier.name.to_string());
                    return Some(self.ast.jsx_element_name_identifier(
                        identifier.span,
                        self.ast.str(tag_name.as_str()),
                    ));
                }

                Some(self.ast.jsx_element_name_identifier_reference(
                    identifier.span,
                    identifier.name.as_str(),
                ))
            }
            Expression::StaticMemberExpression(member) => {
                let object =
                    self.to_jsx_member_object(member.object.clone_in(self.ast.allocator))?;
                let property = self
                    .ast
                    .jsx_identifier(member.property.span, member.property.name.as_str());
                Some(
                    self.ast
                        .jsx_element_name_member_expression(member.span, object, property),
                )
            }
            _ => None,
        }
    }

    fn jsx_tag_from_static_string(&self, span: Span, tag_name: &str) -> JSXElementName<'a> {
        if let Some((namespace, name)) = tag_name.split_once(':') {
            let namespace = self.ast.jsx_identifier(span, self.ast.str(namespace));
            let name = self.ast.jsx_identifier(span, self.ast.str(name));
            return self
                .ast
                .jsx_element_name_namespaced_name(span, namespace, name);
        }

        self.ast
            .jsx_element_name_identifier(span, self.ast.str(tag_name))
    }

    fn dynamic_component_tag(
        &mut self,
        span: Span,
        expression: Expression<'a>,
    ) -> Option<JSXElementName<'a>> {
        let name = self.generate_dynamic_component_name();
        self.pending_dynamic_components
            .push((span, name.clone(), expression));
        Some(
            self.ast
                .jsx_element_name_identifier_reference(span, self.ast.str(&name)),
        )
    }

    fn generate_dynamic_component_name(&mut self) -> String {
        let base = "Component";
        let mut index = 0;
        loop {
            let candidate = if index == 0 {
                base.to_string()
            } else {
                format!("{base}_{index}")
            };
            if !self.dynamic_component_name_occupied(&candidate) {
                if let Some(names) = self.dynamic_component_names.last_mut() {
                    names.insert(candidate.clone());
                }
                return candidate;
            }
            index += 1;
        }
    }

    fn dynamic_component_name_occupied(&self, name: &str) -> bool {
        self.dynamic_component_names
            .iter()
            .rev()
            .any(|names| names.contains(name))
    }

    fn dynamic_component_declaration(
        &self,
        span: Span,
        name: String,
        expression: Expression<'a>,
    ) -> Statement<'a> {
        let mut declarations = self.ast.vec_with_capacity(1);
        declarations.push(
            self.ast.variable_declarator(
                span,
                VariableDeclarationKind::Const,
                BindingPattern::BindingIdentifier(
                    self.ast
                        .alloc_binding_identifier(span, self.ast.str(name.as_str())),
                ),
                None::<oxc_allocator::Box<'a, _>>,
                Some(expression),
                false,
            ),
        );

        Statement::VariableDeclaration(self.ast.alloc_variable_declaration(
            span,
            VariableDeclarationKind::Const,
            declarations,
            false,
        ))
    }

    fn to_jsx_member_object(
        &self,
        expression: Expression<'a>,
    ) -> Option<JSXMemberExpressionObject<'a>> {
        match expression {
            Expression::Identifier(identifier) => {
                Some(self.ast.jsx_member_expression_object_identifier_reference(
                    identifier.span,
                    identifier.name.as_str(),
                ))
            }
            Expression::StaticMemberExpression(member) => {
                let object =
                    self.to_jsx_member_object(member.object.clone_in(self.ast.allocator))?;
                let property = self
                    .ast
                    .jsx_identifier(member.property.span, member.property.name.as_str());
                Some(self.ast.jsx_member_expression_object_member_expression(
                    member.span,
                    object,
                    property,
                ))
            }
            _ => None,
        }
    }

    fn to_jsx_attributes(
        &self,
        props: &Argument<'a>,
    ) -> Option<oxc_allocator::Vec<'a, JSXAttributeItem<'a>>> {
        if matches!(props, Argument::NullLiteral(_)) {
            return Some(self.ast.vec());
        }

        if let Argument::SpreadElement(spread) = props {
            return self.to_jsx_attributes_from_expression(&spread.argument);
        }

        let expression = argument_to_expression(props.clone_in(self.ast.allocator))?;
        self.to_jsx_attributes_from_expression(&expression)
    }

    fn to_jsx_attributes_from_expression(
        &self,
        expression: &Expression<'a>,
    ) -> Option<oxc_allocator::Vec<'a, JSXAttributeItem<'a>>> {
        match expression {
            Expression::CallExpression(call) if is_spread_props_call(call) => {
                let mut attributes = self.ast.vec();
                for argument in &call.arguments {
                    let nested_attributes = self.to_jsx_attributes(argument)?;
                    attributes.extend(nested_attributes);
                }
                Some(attributes)
            }
            Expression::ObjectExpression(object) => {
                let mut attributes = self.ast.vec();
                for property in &object.properties {
                    match property {
                        ObjectPropertyKind::SpreadProperty(spread) => {
                            attributes.push(self.ast.jsx_attribute_item_spread_attribute(
                                spread.span,
                                spread.argument.clone_in(self.ast.allocator),
                            ))
                        }
                        ObjectPropertyKind::ObjectProperty(property) => {
                            if property.computed {
                                let mut properties = self.ast.vec_with_capacity(1);
                                properties.push(ObjectPropertyKind::ObjectProperty(
                                    property.clone_in(self.ast.allocator),
                                ));
                                let object = Expression::ObjectExpression(
                                    self.ast.alloc_object_expression(property.span, properties),
                                );
                                attributes.push(
                                    self.ast
                                        .jsx_attribute_item_spread_attribute(property.span, object),
                                );
                                continue;
                            }

                            let Some(name) = property_key_name(&property.key) else {
                                return None;
                            };

                            let name = jsx_attribute_name(self.ast, property.key.span(), name);
                            let value = if is_true_literal(&property.value) {
                                None
                            } else {
                                Some(self.to_jsx_attribute_value(&property.value))
                            };
                            attributes.push(self.ast.jsx_attribute_item_attribute(
                                property.span,
                                name,
                                value,
                            ));
                        }
                    }
                }
                Some(attributes)
            }
            expression => {
                let mut attributes = self.ast.vec_with_capacity(1);
                attributes.push(self.ast.jsx_attribute_item_spread_attribute(
                    expression.span(),
                    expression.clone_in(self.ast.allocator),
                ));
                Some(attributes)
            }
        }
    }

    fn prepend_key_attribute(
        &self,
        attributes: &mut oxc_allocator::Vec<'a, JSXAttributeItem<'a>>,
        argument: Option<&Argument<'a>>,
    ) {
        let Some(argument) = argument else {
            return;
        };
        if is_undefined_argument(argument) || matches!(argument, Argument::SpreadElement(_)) {
            return;
        }
        let Some(expression) = argument_to_expression(argument.clone_in(self.ast.allocator)) else {
            return;
        };

        let name = self
            .ast
            .jsx_attribute_name_identifier(expression.span(), self.ast.str("key"));
        let value = if let Expression::StringLiteral(string) = &expression {
            if can_be_attribute_string(string.value.as_str()) {
                Some(self.ast.jsx_attribute_value_string_literal(
                    string.span,
                    string.value.as_str(),
                    string.raw,
                ))
            } else {
                Some(self.to_jsx_attribute_value(&expression))
            }
        } else {
            Some(self.to_jsx_attribute_value(&expression))
        };
        attributes.insert(
            0,
            self.ast
                .jsx_attribute_item_attribute(expression.span(), name, value),
        );
    }

    fn take_children_attribute(
        &self,
        attributes: &mut oxc_allocator::Vec<'a, JSXAttributeItem<'a>>,
    ) -> Option<oxc_allocator::Vec<'a, JSXChild<'a>>> {
        let index = attributes
            .iter()
            .position(|attribute| is_jsx_attribute_name(attribute, "children"))?;
        let attribute = attributes.remove(index);
        let JSXAttributeItem::Attribute(attribute) = attribute else {
            return None;
        };
        let value = attribute.value.as_ref()?.clone_in(self.ast.allocator);
        Some(self.jsx_attribute_value_to_children(value))
    }

    fn jsx_attribute_value_to_children(
        &self,
        value: JSXAttributeValue<'a>,
    ) -> oxc_allocator::Vec<'a, JSXChild<'a>> {
        let mut children = self.ast.vec();
        match value {
            JSXAttributeValue::StringLiteral(string) => {
                if let Some(child) = self.string_literal_to_child(&string) {
                    children.push(child);
                }
            }
            JSXAttributeValue::Element(element) => children.push(JSXChild::Element(element)),
            JSXAttributeValue::Fragment(fragment) => children.push(JSXChild::Fragment(fragment)),
            JSXAttributeValue::ExpressionContainer(container) => match &container.expression {
                JSXExpression::ArrayExpression(array) => {
                    for element in &array.elements {
                        if let Some(child) = self.array_element_to_child(element) {
                            children.push(child);
                        }
                    }
                }
                expression => {
                    if let Some(child) = self.jsx_expression_to_child(
                        container.span,
                        expression.clone_in(self.ast.allocator),
                    ) {
                        children.push(child);
                    }
                }
            },
        }
        children
    }

    fn array_element_to_child(&self, element: &ArrayExpressionElement<'a>) -> Option<JSXChild<'a>> {
        match element {
            ArrayExpressionElement::SpreadElement(spread) => Some(
                self.ast
                    .jsx_child_spread(spread.span, spread.argument.clone_in(self.ast.allocator)),
            ),
            ArrayExpressionElement::Elision(_) => None,
            element => self.jsx_expression_to_child(
                element.span(),
                array_element_to_jsx_expression(element.clone_in(self.ast.allocator)),
            ),
        }
    }

    fn jsx_expression_to_child(
        &self,
        span: oxc_span::Span,
        expression: JSXExpression<'a>,
    ) -> Option<JSXChild<'a>> {
        match expression {
            JSXExpression::BooleanLiteral(_) | JSXExpression::NullLiteral(_) => None,
            JSXExpression::Identifier(identifier) if identifier.name == "undefined" => None,
            JSXExpression::UnaryExpression(unary)
                if unary.operator == oxc_syntax::operator::UnaryOperator::Void
                    && matches!(&unary.argument, Expression::NumericLiteral(number) if number.value == 0.0) =>
            {
                None
            }
            JSXExpression::StringLiteral(string) => self.string_literal_to_child(&string),
            JSXExpression::JSXElement(element) => Some(JSXChild::Element(element)),
            JSXExpression::JSXFragment(fragment) => Some(JSXChild::Fragment(fragment)),
            expression => Some(self.ast.jsx_child_expression_container(span, expression)),
        }
    }

    fn string_literal_to_child(
        &self,
        string: &oxc_ast::ast::StringLiteral<'a>,
    ) -> Option<JSXChild<'a>> {
        if can_be_text_child(string.value.as_str()) {
            Some(
                self.ast
                    .jsx_child_text(string.span, string.value.as_str(), None),
            )
        } else {
            Some(self.ast.jsx_child_expression_container(
                string.span,
                JSXExpression::StringLiteral(self.ast.alloc_string_literal(
                    string.span,
                    string.value.as_str(),
                    string.raw,
                )),
            ))
        }
    }

    fn to_jsx_attribute_value(&self, expression: &Expression<'a>) -> JSXAttributeValue<'a> {
        if let Expression::StringLiteral(string) = expression {
            if can_be_attribute_string(string.value.as_str()) {
                return self.ast.jsx_attribute_value_string_literal(
                    string.span,
                    string.value.as_str(),
                    string.raw,
                );
            }
        }

        self.ast.jsx_attribute_value_expression_container(
            expression.span(),
            expression_to_jsx_expression(expression.clone_in(self.ast.allocator)),
        )
    }

    fn to_jsx_child(&self, argument: &Argument<'a>) -> Option<JSXChild<'a>> {
        if matches!(
            argument,
            Argument::NullLiteral(_) | Argument::BooleanLiteral(_)
        ) || is_undefined_argument(argument)
        {
            return None;
        }

        if let Argument::SpreadElement(spread) = argument {
            return Some(
                self.ast
                    .jsx_child_spread(spread.span, spread.argument.clone_in(self.ast.allocator)),
            );
        }

        let expression = argument_to_expression(argument.clone_in(self.ast.allocator))?;
        match expression {
            Expression::JSXElement(element) => Some(JSXChild::Element(element)),
            Expression::JSXFragment(fragment) => Some(JSXChild::Fragment(fragment)),
            Expression::StringLiteral(string) if can_be_text_child(string.value.as_str()) => Some(
                self.ast
                    .jsx_child_text(string.span, string.value.as_str(), None),
            ),
            expression => Some(self.ast.jsx_child_expression_container(
                expression.span(),
                expression_to_jsx_expression(expression),
            )),
        }
    }
}

fn string_tag_declaration<'a>(statement: &'a Statement<'a>) -> Option<(&'a str, &'a str)> {
    let Statement::VariableDeclaration(declaration) = statement else {
        return None;
    };
    if declaration.kind != VariableDeclarationKind::Const || declaration.declarations.len() != 1 {
        return None;
    }

    let declarator = declaration.declarations.first()?;
    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
        return None;
    };
    let Some(init) = &declarator.init else {
        return None;
    };
    let value = match init {
        Expression::StringLiteral(value) => value.value.as_str(),
        Expression::TemplateLiteral(value) => static_template_literal_value(value)?,
        _ => return None,
    };

    Some((identifier.name.as_str(), value))
}

fn static_template_literal_value<'a>(literal: &'a TemplateLiteral<'a>) -> Option<&'a str> {
    literal.single_quasi().map(|value| value.as_str())
}

fn removable_used_string_tag_declaration(
    statement: &Statement,
    used_names: &HashSet<String>,
) -> bool {
    string_tag_declaration(statement).is_some_and(|(name, _)| used_names.contains(name))
}

fn collect_statement_binding_names(statement: &Statement, names: &mut HashSet<String>) {
    match statement {
        Statement::VariableDeclaration(declaration) => {
            for declarator in &declaration.declarations {
                collect_binding_pattern_names(&declarator.id, names);
            }
        }
        Statement::FunctionDeclaration(function) => {
            if let Some(identifier) = &function.id {
                names.insert(identifier.name.to_string());
            }
        }
        Statement::ClassDeclaration(class) => {
            if let Some(identifier) = &class.id {
                names.insert(identifier.name.to_string());
            }
        }
        _ => {}
    }
}

fn collect_binding_pattern_names(pattern: &BindingPattern, names: &mut HashSet<String>) {
    match pattern {
        BindingPattern::BindingIdentifier(identifier) => {
            names.insert(identifier.name.to_string());
        }
        BindingPattern::ObjectPattern(pattern) => {
            for property in &pattern.properties {
                collect_binding_pattern_names(&property.value, names);
            }
            if let Some(rest) = &pattern.rest {
                collect_binding_pattern_names(&rest.argument, names);
            }
        }
        BindingPattern::ArrayPattern(pattern) => {
            for element in pattern.elements.iter().flatten() {
                collect_binding_pattern_names(element, names);
            }
            if let Some(rest) = &pattern.rest {
                collect_binding_pattern_names(&rest.argument, names);
            }
        }
        BindingPattern::AssignmentPattern(pattern) => {
            collect_binding_pattern_names(&pattern.left, names);
        }
    }
}

fn is_spread_props_call(call: &CallExpression) -> bool {
    match without_parentheses(&call.callee) {
        Expression::StaticMemberExpression(member) => {
            let object_name = identifier_name(&member.object);
            (object_name.is_some() && member.property.name == "__spread")
                || (object_name == Some("Object") && member.property.name == "assign")
        }
        _ => false,
    }
}

fn is_jsx_attribute_name(attribute: &JSXAttributeItem, expected: &str) -> bool {
    let JSXAttributeItem::Attribute(attribute) = attribute else {
        return false;
    };
    let JSXAttributeName::Identifier(name) = &attribute.name else {
        return false;
    };
    name.name == expected
}

fn jsx_attribute_name<'a>(
    ast: AstBuilder<'a>,
    span: oxc_span::Span,
    name: &str,
) -> JSXAttributeName<'a> {
    if let Some((namespace, name)) = name.split_once(':') {
        return ast.jsx_attribute_name_namespaced_name(
            span,
            ast.jsx_identifier(span, ast.str(namespace)),
            ast.jsx_identifier(span, ast.str(name)),
        );
    }

    ast.jsx_attribute_name_identifier(span, ast.str(name))
}

fn pascal_case(name: &str) -> String {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    first.to_ascii_uppercase().to_string() + chars.as_str()
}

fn component_name(display_name: &str) -> String {
    let mut name = String::new();
    let mut uppercase_next = true;
    for ch in display_name.chars() {
        if ch == '_' || ch == '$' || ch.is_ascii_alphanumeric() {
            if uppercase_next {
                name.push(ch.to_ascii_uppercase());
                uppercase_next = false;
            } else {
                name.push(ch);
            }
        } else {
            uppercase_next = true;
        }
    }

    if name
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_' || ch == '$')
    {
        name
    } else {
        format!("_{name}")
    }
}

fn capitalization_invalid(tag: &JSXElementName) -> bool {
    match tag {
        JSXElementName::Identifier(identifier) => identifier
            .name
            .as_str()
            .chars()
            .next()
            .is_some_and(|ch| !ch.is_ascii_lowercase()),
        JSXElementName::IdentifierReference(identifier) => identifier
            .name
            .as_str()
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase()),
        _ => false,
    }
}

fn property_key_name<'a>(key: &'a PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(identifier) => Some(identifier.name.as_str()),
        PropertyKey::StringLiteral(string) => Some(string.value.as_str()),
        _ => None,
    }
}

fn identifier_name<'a>(expression: &'a Expression) -> Option<&'a str> {
    let Expression::Identifier(identifier) = without_parentheses(expression) else {
        return None;
    };
    Some(identifier.name.as_str())
}

fn can_be_attribute_string(value: &str) -> bool {
    !value.contains('\\') && !value.contains('"')
}

fn can_be_text_child(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['{', '}', '<', '>', '\r', '\n'])
        && !value.starts_with(char::is_whitespace)
        && !value.ends_with(char::is_whitespace)
}

fn is_true_literal(expression: &Expression) -> bool {
    matches!(expression, Expression::BooleanLiteral(boolean) if boolean.value)
}

fn is_undefined_argument(argument: &Argument) -> bool {
    match argument {
        Argument::Identifier(identifier) => identifier.name == "undefined",
        Argument::UnaryExpression(unary) => {
            unary.operator == oxc_syntax::operator::UnaryOperator::Void
                && matches!(&unary.argument, Expression::NumericLiteral(number) if number.value == 0.0)
        }
        _ => false,
    }
}

fn without_parentheses<'a, 'b>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(parenthesized) => {
            without_parentheses(&parenthesized.expression)
        }
        _ => expression,
    }
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

fn array_element_to_jsx_expression(element: ArrayExpressionElement) -> JSXExpression {
    macro_rules! jsx_expression_variant {
        ($variant:ident, $value:ident) => {
            JSXExpression::$variant($value)
        };
    }

    match element {
        ArrayExpressionElement::SpreadElement(_) | ArrayExpressionElement::Elision(_) => {
            unreachable!("spread and elision array elements are handled before conversion")
        }
        ArrayExpressionElement::BooleanLiteral(value) => {
            jsx_expression_variant!(BooleanLiteral, value)
        }
        ArrayExpressionElement::NullLiteral(value) => jsx_expression_variant!(NullLiteral, value),
        ArrayExpressionElement::NumericLiteral(value) => {
            jsx_expression_variant!(NumericLiteral, value)
        }
        ArrayExpressionElement::BigIntLiteral(value) => {
            jsx_expression_variant!(BigIntLiteral, value)
        }
        ArrayExpressionElement::RegExpLiteral(value) => {
            jsx_expression_variant!(RegExpLiteral, value)
        }
        ArrayExpressionElement::StringLiteral(value) => {
            jsx_expression_variant!(StringLiteral, value)
        }
        ArrayExpressionElement::TemplateLiteral(value) => {
            jsx_expression_variant!(TemplateLiteral, value)
        }
        ArrayExpressionElement::Identifier(value) => jsx_expression_variant!(Identifier, value),
        ArrayExpressionElement::MetaProperty(value) => {
            jsx_expression_variant!(MetaProperty, value)
        }
        ArrayExpressionElement::Super(value) => jsx_expression_variant!(Super, value),
        ArrayExpressionElement::ArrayExpression(value) => {
            jsx_expression_variant!(ArrayExpression, value)
        }
        ArrayExpressionElement::ArrowFunctionExpression(value) => {
            jsx_expression_variant!(ArrowFunctionExpression, value)
        }
        ArrayExpressionElement::AssignmentExpression(value) => {
            jsx_expression_variant!(AssignmentExpression, value)
        }
        ArrayExpressionElement::AwaitExpression(value) => {
            jsx_expression_variant!(AwaitExpression, value)
        }
        ArrayExpressionElement::BinaryExpression(value) => {
            jsx_expression_variant!(BinaryExpression, value)
        }
        ArrayExpressionElement::CallExpression(value) => {
            jsx_expression_variant!(CallExpression, value)
        }
        ArrayExpressionElement::ChainExpression(value) => {
            jsx_expression_variant!(ChainExpression, value)
        }
        ArrayExpressionElement::ClassExpression(value) => {
            jsx_expression_variant!(ClassExpression, value)
        }
        ArrayExpressionElement::ConditionalExpression(value) => {
            jsx_expression_variant!(ConditionalExpression, value)
        }
        ArrayExpressionElement::FunctionExpression(value) => {
            jsx_expression_variant!(FunctionExpression, value)
        }
        ArrayExpressionElement::ImportExpression(value) => {
            jsx_expression_variant!(ImportExpression, value)
        }
        ArrayExpressionElement::LogicalExpression(value) => {
            jsx_expression_variant!(LogicalExpression, value)
        }
        ArrayExpressionElement::NewExpression(value) => {
            jsx_expression_variant!(NewExpression, value)
        }
        ArrayExpressionElement::ObjectExpression(value) => {
            jsx_expression_variant!(ObjectExpression, value)
        }
        ArrayExpressionElement::ParenthesizedExpression(value) => {
            jsx_expression_variant!(ParenthesizedExpression, value)
        }
        ArrayExpressionElement::SequenceExpression(value) => {
            jsx_expression_variant!(SequenceExpression, value)
        }
        ArrayExpressionElement::TaggedTemplateExpression(value) => {
            jsx_expression_variant!(TaggedTemplateExpression, value)
        }
        ArrayExpressionElement::ThisExpression(value) => {
            jsx_expression_variant!(ThisExpression, value)
        }
        ArrayExpressionElement::UnaryExpression(value) => {
            jsx_expression_variant!(UnaryExpression, value)
        }
        ArrayExpressionElement::UpdateExpression(value) => {
            jsx_expression_variant!(UpdateExpression, value)
        }
        ArrayExpressionElement::YieldExpression(value) => {
            jsx_expression_variant!(YieldExpression, value)
        }
        ArrayExpressionElement::PrivateInExpression(value) => {
            jsx_expression_variant!(PrivateInExpression, value)
        }
        ArrayExpressionElement::JSXElement(value) => jsx_expression_variant!(JSXElement, value),
        ArrayExpressionElement::JSXFragment(value) => jsx_expression_variant!(JSXFragment, value),
        ArrayExpressionElement::TSAsExpression(value) => {
            jsx_expression_variant!(TSAsExpression, value)
        }
        ArrayExpressionElement::TSSatisfiesExpression(value) => {
            jsx_expression_variant!(TSSatisfiesExpression, value)
        }
        ArrayExpressionElement::TSTypeAssertion(value) => {
            jsx_expression_variant!(TSTypeAssertion, value)
        }
        ArrayExpressionElement::TSNonNullExpression(value) => {
            jsx_expression_variant!(TSNonNullExpression, value)
        }
        ArrayExpressionElement::TSInstantiationExpression(value) => {
            jsx_expression_variant!(TSInstantiationExpression, value)
        }
        ArrayExpressionElement::ComputedMemberExpression(value) => {
            jsx_expression_variant!(ComputedMemberExpression, value)
        }
        ArrayExpressionElement::StaticMemberExpression(value) => {
            jsx_expression_variant!(StaticMemberExpression, value)
        }
        ArrayExpressionElement::PrivateFieldExpression(value) => {
            jsx_expression_variant!(PrivateFieldExpression, value)
        }
        ArrayExpressionElement::V8IntrinsicExpression(value) => {
            jsx_expression_variant!(V8IntrinsicExpression, value)
        }
    }
}

fn expression_to_jsx_expression(expression: Expression) -> JSXExpression {
    macro_rules! jsx_expression_variant {
        ($variant:ident, $value:ident) => {
            JSXExpression::$variant($value)
        };
    }

    match expression {
        Expression::BooleanLiteral(value) => jsx_expression_variant!(BooleanLiteral, value),
        Expression::NullLiteral(value) => jsx_expression_variant!(NullLiteral, value),
        Expression::NumericLiteral(value) => jsx_expression_variant!(NumericLiteral, value),
        Expression::BigIntLiteral(value) => jsx_expression_variant!(BigIntLiteral, value),
        Expression::RegExpLiteral(value) => jsx_expression_variant!(RegExpLiteral, value),
        Expression::StringLiteral(value) => jsx_expression_variant!(StringLiteral, value),
        Expression::TemplateLiteral(value) => jsx_expression_variant!(TemplateLiteral, value),
        Expression::Identifier(value) => jsx_expression_variant!(Identifier, value),
        Expression::MetaProperty(value) => jsx_expression_variant!(MetaProperty, value),
        Expression::Super(value) => jsx_expression_variant!(Super, value),
        Expression::ArrayExpression(value) => jsx_expression_variant!(ArrayExpression, value),
        Expression::ArrowFunctionExpression(value) => {
            jsx_expression_variant!(ArrowFunctionExpression, value)
        }
        Expression::AssignmentExpression(value) => {
            jsx_expression_variant!(AssignmentExpression, value)
        }
        Expression::AwaitExpression(value) => jsx_expression_variant!(AwaitExpression, value),
        Expression::BinaryExpression(value) => jsx_expression_variant!(BinaryExpression, value),
        Expression::CallExpression(value) => jsx_expression_variant!(CallExpression, value),
        Expression::ChainExpression(value) => jsx_expression_variant!(ChainExpression, value),
        Expression::ClassExpression(value) => jsx_expression_variant!(ClassExpression, value),
        Expression::ConditionalExpression(value) => {
            jsx_expression_variant!(ConditionalExpression, value)
        }
        Expression::FunctionExpression(value) => {
            jsx_expression_variant!(FunctionExpression, value)
        }
        Expression::ImportExpression(value) => jsx_expression_variant!(ImportExpression, value),
        Expression::LogicalExpression(value) => jsx_expression_variant!(LogicalExpression, value),
        Expression::NewExpression(value) => jsx_expression_variant!(NewExpression, value),
        Expression::ObjectExpression(value) => jsx_expression_variant!(ObjectExpression, value),
        Expression::ParenthesizedExpression(value) => {
            jsx_expression_variant!(ParenthesizedExpression, value)
        }
        Expression::SequenceExpression(value) => jsx_expression_variant!(SequenceExpression, value),
        Expression::TaggedTemplateExpression(value) => {
            jsx_expression_variant!(TaggedTemplateExpression, value)
        }
        Expression::ThisExpression(value) => jsx_expression_variant!(ThisExpression, value),
        Expression::UnaryExpression(value) => jsx_expression_variant!(UnaryExpression, value),
        Expression::UpdateExpression(value) => jsx_expression_variant!(UpdateExpression, value),
        Expression::YieldExpression(value) => jsx_expression_variant!(YieldExpression, value),
        Expression::PrivateInExpression(value) => {
            jsx_expression_variant!(PrivateInExpression, value)
        }
        Expression::JSXElement(value) => jsx_expression_variant!(JSXElement, value),
        Expression::JSXFragment(value) => jsx_expression_variant!(JSXFragment, value),
        Expression::TSAsExpression(value) => jsx_expression_variant!(TSAsExpression, value),
        Expression::TSSatisfiesExpression(value) => {
            jsx_expression_variant!(TSSatisfiesExpression, value)
        }
        Expression::TSTypeAssertion(value) => jsx_expression_variant!(TSTypeAssertion, value),
        Expression::TSNonNullExpression(value) => {
            jsx_expression_variant!(TSNonNullExpression, value)
        }
        Expression::TSInstantiationExpression(value) => {
            jsx_expression_variant!(TSInstantiationExpression, value)
        }
        Expression::ComputedMemberExpression(value) => {
            jsx_expression_variant!(ComputedMemberExpression, value)
        }
        Expression::StaticMemberExpression(value) => {
            jsx_expression_variant!(StaticMemberExpression, value)
        }
        Expression::PrivateFieldExpression(value) => {
            jsx_expression_variant!(PrivateFieldExpression, value)
        }
        Expression::V8IntrinsicExpression(value) => {
            jsx_expression_variant!(V8IntrinsicExpression, value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{define_ast_inline_test, define_ast_inline_test_with_params};
    use wakaru_core::source::TransformationParams;

    #[test]
    fn restores_simple_classic_jsx() {
        define_ast_inline_test(transform_ast)(
            r#"
function fn() {
  return React.createElement("div", {
    className: "flex flex-col",
    num: 1,
    foo: bar,
    disabled: true,
  });
}
"#,
            r#"
function fn() {
  return <div className="flex flex-col" num={1} foo={bar} disabled />;
}
"#,
        );
    }

    #[test]
    fn restores_custom_classic_pragma() {
        define_ast_inline_test_with_params(
            transform_ast,
            TransformationParams {
                un_jsx_pragma: Some("h".to_string()),
                ..TransformationParams::default()
            },
        )(
            r#"
function fn() {
  return h("div", { id: "app" }, "Hello");
}
React.createElement("span", null);
"#,
            r#"
function fn() {
  return <div id="app">Hello</div>;
}
React.createElement("span", null);
"#,
        );
    }

    #[test]
    fn restores_custom_dotted_pragma_and_fragment() {
        define_ast_inline_test_with_params(
            transform_ast,
            TransformationParams {
                un_jsx_pragma: Some("Preact.h".to_string()),
                un_jsx_pragma_frag: Some("Preact.Frag".to_string()),
                ..TransformationParams::default()
            },
        )(
            r#"
function fn() {
  return Preact.h(Frag, null, Preact.h("span", null, "Hello"));
}
"#,
            r#"
function fn() {
  return <><span>Hello</span></>;
}
"#,
        );
    }

    #[test]
    fn restores_children_and_nested_elements() {
        define_ast_inline_test(transform_ast)(
            r#"
function fn() {
  return React.createElement("div", null, child, React.createElement("span", null, "Hello"));
}
"#,
            r#"
function fn() {
  return <div>{child}<span>Hello</span></div>;
}
"#,
        );
    }

    #[test]
    fn handles_component_member_and_spread_props() {
        define_ast_inline_test(transform_ast)(
            r#"
React.createElement(Button, { variant: "contained" }, "Hello");
React.createElement(mui.Button, { ...props, foo: "bar" });
React.createElement("div", wrap(props));
React.createElement("div", React.__spread({ key: "1" }, { className: "flex flex-col" }));
React.createElement("div", Object.assign({ key: "1" }, { className: "flex flex-col" }));
React.createElement("div", ...{ key: "1", className: "flex flex-col" });
"#,
            r#"
<Button variant="contained">Hello</Button>;
<mui.Button {...props} foo="bar" />;
<div {...wrap(props)} />;
<div key="1" className="flex flex-col" />;
<div key="1" className="flex flex-col" />;
<div key="1" className="flex flex-col" />;
"#,
        );
    }

    #[test]
    fn drops_pure_annotations_on_converted_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
var div = /*#__PURE__*/React.createElement(Component, {
  ...props,
  foo: "bar"
});
"#,
            r#"
var div = <Component {...props} foo="bar" />;
"#,
        );
    }

    #[test]
    fn restores_automatic_runtime_jsx() {
        define_ast_inline_test(transform_ast)(
            r#"
const Foo = () => {
  return _jsxs("div", {
    children: [_jsx("p", {
      id: "a"
    }, void 0), _jsx("p", {
      children: "bar"
    }, "b"), _jsx("p", {
      children: baz
    }, c)]
  });
};
"#,
            r#"
const Foo = () => {
  return <div><p id="a" /><p key="b">bar</p><p key={c}>{baz}</p></div>;
};
"#,
        );
    }

    #[test]
    fn hoists_dynamic_component_tags() {
        define_ast_inline_test(transform_ast)(
            r#"
function fn() {
  return React.createElement(r ? "a" : "div", null, "Hello");
}
function fn2() {
  const Component = Button;
  return React.createElement(r ? "a" : "div", null, "Hello");
}
"#,
            r#"
function fn() {
  const Component = r ? "a" : "div";
  return <Component>Hello</Component>;
}
function fn2() {
  const Component = Button;
  const Component_1 = r ? "a" : "div";
  return <Component_1>Hello</Component_1>;
}
"#,
        );
    }

    #[test]
    fn inlines_constant_string_tags() {
        define_ast_inline_test(transform_ast)(
            r#"
function fn() {
  const Name = "div";
  return React.createElement(Name, null);
}
"#,
            r#"
function fn() {
  return <div />;
}
"#,
        );
    }

    #[test]
    fn inlines_constant_template_string_tags() {
        define_ast_inline_test(transform_ast)(
            r#"
function fn() {
  const Name = `div`;
  return React.createElement(Name, null);
}
b = _jsxs(`div`, {
  className: `flex flex-wrap items-center justify-center gap-3`,
  children: [v, y]
});
"#,
            r#"
function fn() {
  return <div />;
}
b = <div className={`flex flex-wrap items-center justify-center gap-3`}>{v}{y}</div>;
"#,
        );
    }

    #[test]
    fn renames_lowercase_component_bindings() {
        define_ast_inline_test(transform_ast)(
            r#"
function foo() {}
React.createElement(foo, null);
function fn() {
  function bar() {}
  const Bar = 1;
  return React.createElement(bar, null);
}
"#,
            r#"
function Foo() {}
<Foo />;
function fn() {
  function Bar_1() {}
  const Bar = 1;
  return <Bar_1 />;
}
"#,
        );
    }

    #[test]
    fn renames_components_from_display_name() {
        define_ast_inline_test(transform_ast)(
            r#"
var s = React.createElement("div", null);
s.displayName = "Test";
var t = () => React.createElement("div", null);
t.displayName = "Foo-Bar";
var Bar = React.createElement("div", null, React.createElement(s, null));
var Baz = () => React.createElement("div", null, React.createElement(t, null));
"#,
            r#"
var Test = <div />;
Test.displayName = "Test";
var FooBar = () => <div />;
FooBar.displayName = "Foo-Bar";
var Bar = <div><Test /></div>;
var Baz = () => <div><FooBar /></div>;
"#,
        );
    }

    #[test]
    fn restores_namespaced_jsx_and_computed_props() {
        define_ast_inline_test(transform_ast)(
            r#"
jsx("f:image", {
  "n:attr": true
});
React.createElement("pre", {
  ["__proto__"]: null
});
React.createElement("code", {
  [__proto__]: null
});
"#,
            r#"
<f:image n:attr />;
<pre {...{ ["__proto__"]: null }} />;
<code {...{ [__proto__]: null }} />;
"#,
        );
    }

    #[test]
    fn restores_classic_fragments_without_attributes() {
        define_ast_inline_test(transform_ast)(
            r#"
React.createElement(React.Fragment, null, React.createElement("span", null, "Hello"));
React.createElement(Fragment, null, "World");
"#,
            r#"
<><span>Hello</span></>;
<>World</>;
"#,
        );
    }

    #[test]
    fn keeps_fragment_component_when_attributes_are_present() {
        define_ast_inline_test(transform_ast)(
            r#"
React.createElement(React.Fragment, { key: "a" }, React.createElement("span", null));
"#,
            r#"
<React.Fragment key="a"><span /></React.Fragment>;
"#,
        );
    }

    #[test]
    fn leaves_bad_capitalization_and_document_create_element() {
        define_ast_inline_test(transform_ast)(
            r#"
React.createElement(foo, null);
React.createElement("Foo", null);
React.createElement(_foo, null);
React.createElement("_foo", null);
React.createElement(foo.bar, null);
document.createElement("div", null);
window.document.createElement("div", attrs);
"#,
            r#"
React.createElement(foo, null);
React.createElement("Foo", null);
<_foo />;
React.createElement("_foo", null);
<foo.bar />;
document.createElement("div", null);
window.document.createElement("div", attrs);
"#,
        );
    }
}
