use oxc_ast::ast::{
    Argument, AssignmentExpression, AssignmentOperator, AssignmentTarget, CallExpression,
    Expression, ExpressionStatement, Statement, UnaryOperator,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    EsModuleFlagRemover.visit_program(&mut source.program);
    Ok(())
}

struct EsModuleFlagRemover;

impl<'a> VisitMut<'a> for EsModuleFlagRemover {
    fn visit_statements(&mut self, statements: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        statements.retain(|statement| !is_esmodule_flag_statement(statement));
        walk_mut::walk_statements(self, statements);
    }
}

fn is_esmodule_flag_statement(statement: &Statement) -> bool {
    match statement {
        Statement::ExpressionStatement(statement) => {
            is_esmodule_flag_expression_statement(statement)
        }
        _ => false,
    }
}

fn is_esmodule_flag_expression_statement(statement: &ExpressionStatement) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn removes_es5_define_property_flags() {
        let inline_test = define_ast_inline_test(transform_ast);

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
        let inline_test = define_ast_inline_test(transform_ast);

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
        let inline_test = define_ast_inline_test(transform_ast);

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
