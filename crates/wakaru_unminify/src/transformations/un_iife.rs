use std::collections::HashMap;

use oxc_ast::{
    ast::{
        Argument, BindingIdentifier, BindingPattern, CallExpression, Expression, FunctionBody,
        IdentifierReference, Program, Statement, VariableDeclarationKind,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk, Visit, VisitMut};
use oxc_semantic::{Scoping, SemanticBuilder, SymbolId};
use oxc_span::Span;
use oxc_syntax::operator::UnaryOperator;
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();

    let mut transformer = IifeTransformer {
        ast: AstBuilder::new(source.allocator),
        renames: HashMap::new(),
    };
    transformer.transform_program(&mut source.program);

    let mut renamer = SymbolRenamer {
        ast: AstBuilder::new(source.allocator),
        scoping: &scoping,
        renames: transformer.renames,
    };
    renamer.visit_program(&mut source.program);

    Ok(())
}

struct IifeTransformer<'a> {
    ast: AstBuilder<'a>,
    renames: HashMap<SymbolId, String>,
}

impl<'a> IifeTransformer<'a> {
    fn transform_program(&mut self, program: &mut Program<'a>) {
        for statement in &mut program.body {
            let Statement::ExpressionStatement(statement) = statement else {
                continue;
            };

            self.transform_top_level_expression(&mut statement.expression);
        }
    }

    fn transform_top_level_expression(&mut self, expression: &mut Expression<'a>) {
        match expression {
            Expression::CallExpression(call) => self.transform_call(call),
            Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::LogicalNot => {
                if let Expression::CallExpression(call) = &mut unary.argument {
                    self.transform_call(call);
                }
            }
            Expression::ParenthesizedExpression(parenthesized) => {
                self.transform_top_level_expression(&mut parenthesized.expression);
            }
            _ => {}
        }
    }

    fn transform_call(&mut self, call: &mut CallExpression<'a>) {
        let Some(mut callee) = IifeCalleeMut::from_expression(&mut call.callee) else {
            return;
        };

        let arguments_used = callee
            .body()
            .is_some_and(|body| function_uses_arguments(&*body));
        let parameter_len = callee.params().items.len();
        if parameter_len == 0 {
            return;
        }

        for index in (0..parameter_len).rev() {
            let Some(parameter_name) = parameter_name(callee.params(), index) else {
                continue;
            };
            if parameter_name.len() != 1 {
                continue;
            }

            if let Some((symbol_id, new_name)) =
                argument_identifier_rename(callee.params(), &call.arguments, index, &parameter_name)
            {
                self.renames.insert(symbol_id, new_name);
                continue;
            }

            if !arguments_used {
                self.move_literal_argument_to_const(
                    &mut callee,
                    &mut call.arguments,
                    index,
                    &parameter_name,
                );
            }
        }
    }

    fn move_literal_argument_to_const(
        &self,
        callee: &mut IifeCalleeMut<'_, 'a>,
        arguments: &mut oxc_allocator::Vec<'a, Argument<'a>>,
        index: usize,
        parameter_name: &str,
    ) {
        if index >= arguments.len() || !argument_is_value_literal(&arguments[index]) {
            return;
        }

        let argument = arguments.remove(index);
        let Some(init) = argument_to_expression(argument) else {
            return;
        };
        callee.params().items.remove(index);

        let Some(body) = callee.body() else {
            return;
        };
        body.statements.insert(
            0,
            const_declaration_statement(self.ast, parameter_name, init),
        );
    }
}

enum IifeCalleeMut<'b, 'a> {
    Function(&'b mut oxc_ast::ast::Function<'a>),
    Arrow(&'b mut oxc_ast::ast::ArrowFunctionExpression<'a>),
}

impl<'b, 'a> IifeCalleeMut<'b, 'a> {
    fn from_expression(expression: &'b mut Expression<'a>) -> Option<Self> {
        match expression {
            Expression::FunctionExpression(function) => Some(Self::Function(function)),
            Expression::ArrowFunctionExpression(arrow) if !arrow.expression => {
                Some(Self::Arrow(arrow))
            }
            Expression::ParenthesizedExpression(parenthesized) => {
                Self::from_expression(&mut parenthesized.expression)
            }
            _ => None,
        }
    }

    fn params(&mut self) -> &mut oxc_ast::ast::FormalParameters<'a> {
        match self {
            Self::Function(function) => &mut function.params,
            Self::Arrow(arrow) => &mut arrow.params,
        }
    }

    fn body(&mut self) -> Option<&mut FunctionBody<'a>> {
        match self {
            Self::Function(function) => function.body.as_deref_mut(),
            Self::Arrow(arrow) => Some(&mut arrow.body),
        }
    }
}

fn parameter_name(params: &oxc_ast::ast::FormalParameters, index: usize) -> Option<String> {
    let parameter = params.items.get(index)?;
    let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
        return None;
    };

    Some(identifier.name.as_str().to_string())
}

fn argument_identifier_rename(
    params: &oxc_ast::ast::FormalParameters,
    arguments: &oxc_allocator::Vec<Argument>,
    index: usize,
    old_name: &str,
) -> Option<(SymbolId, String)> {
    let parameter = params.items.get(index)?;
    let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
        return None;
    };
    let Some(Argument::Identifier(argument)) = arguments.get(index) else {
        return None;
    };

    let new_name = argument.name.as_str();
    if new_name == old_name || new_name.len() <= 1 {
        return None;
    }

    Some((identifier.symbol_id.get()?, new_name.to_string()))
}

fn function_uses_arguments(body: &FunctionBody) -> bool {
    let mut scanner = ArgumentsUsageScanner { found: false };
    scanner.visit_function_body(body);
    scanner.found
}

struct ArgumentsUsageScanner {
    found: bool,
}

impl<'a> Visit<'a> for ArgumentsUsageScanner {
    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        if identifier.name == "arguments" {
            self.found = true;
            return;
        }

        walk::walk_identifier_reference(self, identifier);
    }
}

fn argument_is_value_literal(argument: &Argument) -> bool {
    matches!(
        argument,
        Argument::BooleanLiteral(_)
            | Argument::NullLiteral(_)
            | Argument::NumericLiteral(_)
            | Argument::BigIntLiteral(_)
            | Argument::StringLiteral(_)
            | Argument::RegExpLiteral(_)
    )
}

fn const_declaration_statement<'a>(
    ast: AstBuilder<'a>,
    name: &str,
    init: Expression<'a>,
) -> Statement<'a> {
    let mut declarations = ast.vec_with_capacity(1);
    declarations.push(ast.variable_declarator(
        Span::default(),
        VariableDeclarationKind::Const,
        ast.binding_pattern_binding_identifier(Span::default(), ast.ident(name)),
        None::<oxc_allocator::Box<'a, oxc_ast::ast::TSTypeAnnotation<'a>>>,
        Some(init),
        false,
    ));

    Statement::VariableDeclaration(ast.alloc_variable_declaration(
        Span::default(),
        VariableDeclarationKind::Const,
        declarations,
        false,
    ))
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
    fn renames_single_character_params_from_identifier_arguments() {
        define_ast_inline_test(transform_ast)(
            "
(function(i, s, o) {
  i.x = s.createElement(o);
})(window, document, 'script');
",
            "
(function(window, document) {
  const o = \"script\";
  window.x = document.createElement(o);
})(window, document);
",
        );
    }

    #[test]
    fn moves_literal_arguments_for_bang_iife() {
        define_ast_inline_test(transform_ast)(
            "
!function(i, s, o, g) {
  i.x = s.createElement(o);
  i.y = g;
}(window, document, 'script', 'url');
",
            "
!function(window, document) {
  const o = \"script\";
  const g = \"url\";
  window.x = document.createElement(o);
  window.y = g;
}(window, document);
",
        );
    }

    #[test]
    fn does_not_move_literals_when_arguments_is_used() {
        define_ast_inline_test(transform_ast)(
            "
(function(i, o) {
  console.log(arguments);
  i.x = o;
})(window, 'script');
",
            "
(function(window, o) {
  console.log(arguments);
  window.x = o;
})(window, \"script\");
",
        );
    }

    #[test]
    fn skips_long_params_and_short_arguments() {
        define_ast_inline_test(transform_ast)(
            "
((win, s, a) => {
  win.x = s.createElement('script');
  a.src = 'url';
})(window, document);

(function(i, s, a) {
  i.x = s.createElement('script');
})(w, document);
",
            "
((win, document, a) => {
  win.x = document.createElement(\"script\");
  a.src = \"url\";
})(window, document);
(function(i, document, a) {
  i.x = document.createElement(\"script\");
})(w, document);
",
        );
    }

    #[test]
    fn ignores_nested_iifes() {
        define_ast_inline_test(transform_ast)(
            "
function outer() {
  (function(i) {
    i.x = 1;
  })(window);
}
",
            "
function outer() {
  (function(i) {
    i.x = 1;
  })(window);
}
",
        );
    }
}
