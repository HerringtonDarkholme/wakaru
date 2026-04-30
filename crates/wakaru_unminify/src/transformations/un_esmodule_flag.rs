use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, AssignmentExpression, AssignmentOperator, AssignmentTarget, CallExpression,
    Expression, ExpressionStatement, UnaryOperator,
};
use oxc_ast_visit::{walk, Visit};
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};
use wakaru_core::diagnostics::{Diagnostic, Result, WakaruError};
use wakaru_core::source::SourceFile;

pub fn transform(source: &SourceFile) -> Result<String> {
    let source_type = SourceType::from_path(&source.path)
        .unwrap_or_else(|_| SourceType::default().with_jsx(true));
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, &source.code, source_type).parse();

    if !ret.errors.is_empty() || ret.panicked {
        let diagnostics = ret
            .errors
            .into_iter()
            .map(|err| Diagnostic::error(format!("{err:?}")).with_path(source.path.clone()))
            .collect();

        return Err(WakaruError::with_diagnostics(
            format!("failed to parse {}", source.path.display()),
            diagnostics,
        ));
    }

    let mut collector = EsModuleFlagCollector::default();
    collector.visit_program(&ret.program);

    Ok(remove_statements(&source.code, &collector.spans))
}

#[derive(Default)]
struct EsModuleFlagCollector {
    spans: Vec<Span>,
}

impl<'a> Visit<'a> for EsModuleFlagCollector {
    fn visit_expression_statement(&mut self, statement: &ExpressionStatement<'a>) {
        if is_esmodule_flag_statement(statement) {
            self.spans.push(statement.span);
            return;
        }

        walk::walk_expression_statement(self, statement);
    }
}

fn is_esmodule_flag_statement(statement: &ExpressionStatement) -> bool {
    match &statement.expression {
        Expression::CallExpression(call) => is_define_property_call(call),
        Expression::AssignmentExpression(assignment) => is_esmodule_flag_assignment(assignment),
        _ => false,
    }
}

fn is_define_property_call(call: &CallExpression) -> bool {
    if !is_static_member_expression(&call.callee, "Object", "defineProperty") {
        return false;
    }

    matches!(
        call.arguments.as_slice(),
        [target, property, ..] if is_export_object_argument(target) && is_string_argument(property, "__esModule")
    )
}

fn is_esmodule_flag_assignment(assignment: &AssignmentExpression) -> bool {
    if assignment.operator != AssignmentOperator::Assign {
        return false;
    }

    is_esmodule_flag_assignment_target(&assignment.left) && is_loose_true(&assignment.right)
}

fn is_esmodule_flag_assignment_target(target: &AssignmentTarget) -> bool {
    match target {
        AssignmentTarget::StaticMemberExpression(member) => {
            is_export_object(&member.object) && member.property.name.as_str() == "__esModule"
        }
        AssignmentTarget::ComputedMemberExpression(member) => {
            is_export_object(&member.object)
                && is_string_expression(&member.expression, "__esModule")
        }
        _ => false,
    }
}

fn is_export_object_argument(argument: &Argument) -> bool {
    match argument {
        Argument::Identifier(ident) => ident.name.as_str() == "exports",
        Argument::StaticMemberExpression(member) => {
            is_identifier_expression(&member.object, "module")
                && member.property.name.as_str() == "exports"
        }
        _ => false,
    }
}

fn is_export_object(expression: &Expression) -> bool {
    is_identifier_expression(expression, "exports")
        || is_static_member_expression(expression, "module", "exports")
}

fn is_static_member_expression(expression: &Expression, object: &str, property: &str) -> bool {
    match expression {
        Expression::StaticMemberExpression(member) => {
            is_identifier_expression(&member.object, object)
                && member.property.name.as_str() == property
        }
        _ => false,
    }
}

fn is_identifier_expression(expression: &Expression, name: &str) -> bool {
    matches!(expression, Expression::Identifier(ident) if ident.name.as_str() == name)
}

fn is_string_argument(argument: &Argument, value: &str) -> bool {
    matches!(argument, Argument::StringLiteral(literal) if literal.value.as_str() == value)
}

fn is_string_expression(expression: &Expression, value: &str) -> bool {
    matches!(expression, Expression::StringLiteral(literal) if literal.value.as_str() == value)
}

fn is_loose_true(expression: &Expression) -> bool {
    match expression {
        Expression::BooleanLiteral(literal) => literal.value,
        Expression::UnaryExpression(unary) => {
            unary.operator == UnaryOperator::LogicalNot
                && matches!(&unary.argument, Expression::NumericLiteral(number) if number.value == 0.0)
        }
        _ => false,
    }
}

fn remove_statements(source: &str, spans: &[Span]) -> String {
    if spans.is_empty() {
        return source.to_string();
    }

    let mut ranges = spans
        .iter()
        .map(|span| removal_range(source, span))
        .collect::<Vec<_>>();
    ranges.sort_unstable_by_key(|(start, _)| *start);

    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;

    for (start, end) in ranges {
        if start < cursor {
            continue;
        }

        output.push_str(&source[cursor..start]);
        cursor = end;
    }

    output.push_str(&source[cursor..]);
    output
}

fn removal_range(source: &str, span: &Span) -> (usize, usize) {
    let mut start = span.start as usize;
    let mut end = span.end as usize;
    let bytes = source.as_bytes();

    while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
        start -= 1;
    }

    while end < bytes.len() && matches!(bytes[end], b' ' | b'\t') {
        end += 1;
    }

    if end < bytes.len() && bytes[end] == b';' {
        end += 1;
        while end < bytes.len() && matches!(bytes[end], b' ' | b'\t') {
            end += 1;
        }
    }

    if end + 1 < bytes.len() && bytes[end] == b'\r' && bytes[end + 1] == b'\n' {
        end += 2;
    } else if end < bytes.len() && matches!(bytes[end], b'\n' | b'\r') {
        end += 1;
    }

    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_inline_test;

    #[test]
    fn removes_es5_define_property_flags() {
        let inline_test = define_inline_test(transform);

        inline_test(
            r#"
Object.defineProperty(exports, "__esModule", {
  value: true
});
Object.defineProperty(module.exports, "__esModule", {
  value: !0
});
"#,
            "
",
        );
    }

    #[test]
    fn removes_es3_assignment_flags() {
        let inline_test = define_inline_test(transform);

        inline_test(
            r#"
exports.__esModule = !0;
exports.__esModule = true;
exports["__esModule"] = true;
module.exports.__esModule = !0;
module.exports.__esModule = true;
module.exports["__esModule"] = true;
"#,
            "
",
        );
    }

    #[test]
    fn leaves_similar_non_flags() {
        let inline_test = define_inline_test(transform);

        inline_test(
            r#"
Object.defineProperty(window, "__esModule", { value: true });
exports.__esModule = false;
exports.other = true;
"#,
            r#"
Object.defineProperty(window, "__esModule", { value: true });
exports.__esModule = false;
exports.other = true;
"#,
        );
    }
}
