use oxc_ast::ast::Expression;

pub fn is_helper_function_call(expression: &Expression, helper_name: &str) -> bool {
    let Expression::CallExpression(call) = expression else {
        return false;
    };

    is_helper_callee(&call.callee, helper_name)
}

pub fn is_helper_callee(expression: &Expression, helper_name: &str) -> bool {
    let callee = unwrapped_sequence_callee(expression);

    if let Some((helper, helper_prop)) = helper_name.split_once('.') {
        let Expression::StaticMemberExpression(member) = callee else {
            return false;
        };

        return is_identifier(&member.object, helper)
            && member.property.name.as_str() == helper_prop
            || matches!(
                &member.object,
                Expression::StaticMemberExpression(object)
                    if is_identifier(&object.object, helper)
                        && object.property.name.as_str() == "default"
                        && member.property.name.as_str() == helper_prop
            );
    }

    is_identifier(callee, helper_name)
        || matches!(
            callee,
            Expression::StaticMemberExpression(member)
                if is_identifier(&member.object, helper_name)
                    && member.property.name.as_str() == "default"
        )
}

fn unwrapped_sequence_callee<'a, 'b>(expression: &'b Expression<'a>) -> &'b Expression<'a> {
    let mut expression = expression;

    loop {
        match expression {
            Expression::ParenthesizedExpression(parenthesized) => {
                expression = &parenthesized.expression;
            }
            Expression::SequenceExpression(sequence)
                if sequence.expressions.len() == 2
                    && matches!(&sequence.expressions[0], Expression::NumericLiteral(number) if number.value == 0.0) =>
            {
                expression = &sequence.expressions[1];
            }
            _ => return expression,
        }
    }
}

fn is_identifier(expression: &Expression, name: &str) -> bool {
    matches!(expression, Expression::Identifier(identifier) if identifier.name.as_str() == name)
}
