use oxc_ast::{
    ast::{CallExpression, Expression},
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::{GetSpan, Span};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut restorer = BuiltinPrototypeRestorer {
        ast: AstBuilder::new(source.allocator),
    };

    restorer.visit_program(&mut source.program);

    Ok(())
}

struct BuiltinPrototypeRestorer<'a> {
    ast: AstBuilder<'a>,
}

impl<'a> VisitMut<'a> for BuiltinPrototypeRestorer<'a> {
    fn visit_call_expression(&mut self, call: &mut CallExpression<'a>) {
        walk_mut::walk_call_expression(self, call);

        let Some(replacement) = builtin_prototype_replacement(&call.callee) else {
            return;
        };

        call.callee = self.prototype_call_callee(call.callee.span(), replacement);
    }
}

impl<'a> BuiltinPrototypeRestorer<'a> {
    fn prototype_call_callee(
        &self,
        span: Span,
        replacement: PrototypeReplacement,
    ) -> Expression<'a> {
        let prototype = self.ast.expression_identifier(span, replacement.prototype);
        let prototype = self.static_member(prototype, span, "prototype");
        let method = self.static_member(prototype, replacement.method_span, &replacement.method);

        self.static_member(method, replacement.dispatch_span, &replacement.dispatch)
    }

    fn static_member(&self, object: Expression<'a>, span: Span, property: &str) -> Expression<'a> {
        let property = self
            .ast
            .identifier_name(span, self.ast.allocator.alloc_str(property));

        Expression::StaticMemberExpression(
            self.ast
                .alloc_static_member_expression(span, object, property, false),
        )
    }
}

struct PrototypeReplacement {
    prototype: &'static str,
    method: String,
    method_span: Span,
    dispatch: String,
    dispatch_span: Span,
}

fn builtin_prototype_replacement(callee: &Expression) -> Option<PrototypeReplacement> {
    let Expression::StaticMemberExpression(dispatch_member) = callee else {
        return None;
    };

    if dispatch_member.optional || !is_call_or_apply(dispatch_member.property.name.as_str()) {
        return None;
    }

    let Expression::StaticMemberExpression(method_member) = &dispatch_member.object else {
        return None;
    };

    if method_member.optional {
        return None;
    }

    let method = method_member.property.name.as_str();
    let prototype = builtin_prototype_name(&method_member.object, method)?;

    Some(PrototypeReplacement {
        prototype,
        method: method.to_string(),
        method_span: method_member.property.span,
        dispatch: dispatch_member.property.name.as_str().to_string(),
        dispatch_span: dispatch_member.property.span,
    })
}

fn builtin_prototype_name(object: &Expression, method: &str) -> Option<&'static str> {
    let object = unparenthesized_expression(object);

    match object {
        Expression::ArrayExpression(array)
            if array.elements.is_empty() && is_array_prototype_method(method) =>
        {
            Some("Array")
        }
        Expression::NumericLiteral(number)
            if number.value == 0.0 && is_number_prototype_method(method) =>
        {
            Some("Number")
        }
        Expression::ObjectExpression(object)
            if object.properties.is_empty() && is_object_prototype_method(method) =>
        {
            Some("Object")
        }
        Expression::RegExpLiteral(regex)
            if !regex.regex.pattern.text.as_str().is_empty()
                && is_regexp_prototype_method(method) =>
        {
            Some("RegExp")
        }
        Expression::StringLiteral(string)
            if string.value.as_str().is_empty() && is_string_prototype_method(method) =>
        {
            Some("String")
        }
        Expression::FunctionExpression(_) | Expression::ArrowFunctionExpression(_)
            if is_function_prototype_method(method) =>
        {
            Some("Function")
        }
        _ => None,
    }
}

fn unparenthesized_expression<'a, 'b>(mut expression: &'b Expression<'a>) -> &'b Expression<'a> {
    while let Expression::ParenthesizedExpression(parenthesized) = expression {
        expression = &parenthesized.expression;
    }

    expression
}

fn is_call_or_apply(method: &str) -> bool {
    matches!(method, "call" | "apply")
}

fn is_array_prototype_method(method: &str) -> bool {
    is_object_prototype_method(method)
        || matches!(
            method,
            "at" | "concat"
                | "copyWithin"
                | "entries"
                | "every"
                | "fill"
                | "filter"
                | "find"
                | "findIndex"
                | "findLast"
                | "findLastIndex"
                | "flat"
                | "flatMap"
                | "forEach"
                | "includes"
                | "indexOf"
                | "join"
                | "keys"
                | "lastIndexOf"
                | "map"
                | "pop"
                | "push"
                | "reduce"
                | "reduceRight"
                | "reverse"
                | "shift"
                | "slice"
                | "some"
                | "sort"
                | "splice"
                | "toLocaleString"
                | "toReversed"
                | "toSorted"
                | "toSpliced"
                | "unshift"
                | "values"
                | "with"
        )
}

fn is_number_prototype_method(method: &str) -> bool {
    is_object_prototype_method(method)
        || matches!(
            method,
            "toExponential" | "toFixed" | "toLocaleString" | "toPrecision" | "toString" | "valueOf"
        )
}

fn is_object_prototype_method(method: &str) -> bool {
    matches!(
        method,
        "constructor"
            | "__defineGetter__"
            | "__defineSetter__"
            | "hasOwnProperty"
            | "__lookupGetter__"
            | "__lookupSetter__"
            | "isPrototypeOf"
            | "propertyIsEnumerable"
            | "toLocaleString"
            | "toString"
            | "valueOf"
            | "__proto__"
    )
}

fn is_regexp_prototype_method(method: &str) -> bool {
    is_object_prototype_method(method)
        || matches!(
            method,
            "compile"
                | "dotAll"
                | "exec"
                | "flags"
                | "global"
                | "hasIndices"
                | "ignoreCase"
                | "multiline"
                | "source"
                | "sticky"
                | "test"
                | "unicode"
                | "unicodeSets"
        )
}

fn is_string_prototype_method(method: &str) -> bool {
    is_object_prototype_method(method)
        || matches!(
            method,
            "anchor"
                | "at"
                | "big"
                | "blink"
                | "bold"
                | "charAt"
                | "charCodeAt"
                | "codePointAt"
                | "concat"
                | "endsWith"
                | "fixed"
                | "fontcolor"
                | "fontsize"
                | "includes"
                | "indexOf"
                | "isWellFormed"
                | "italics"
                | "lastIndexOf"
                | "link"
                | "localeCompare"
                | "match"
                | "matchAll"
                | "normalize"
                | "padEnd"
                | "padStart"
                | "repeat"
                | "replace"
                | "replaceAll"
                | "search"
                | "slice"
                | "small"
                | "split"
                | "startsWith"
                | "strike"
                | "sub"
                | "substr"
                | "substring"
                | "sup"
                | "toLocaleLowerCase"
                | "toLocaleUpperCase"
                | "toLowerCase"
                | "toString"
                | "toUpperCase"
                | "toWellFormed"
                | "trim"
                | "trimEnd"
                | "trimLeft"
                | "trimRight"
                | "trimStart"
                | "valueOf"
        )
}

fn is_function_prototype_method(method: &str) -> bool {
    is_object_prototype_method(method) || matches!(method, "apply" | "bind" | "call" | "toString")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn restores_builtin_prototype_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
[].splice.apply(a, [1, 2, b, c]);
(function() {}).call.apply(console.log, console, ["foo"]),
(() => {}).call.apply(console.log,console,["foo"]);
0..toFixed.call(Math.PI, 2);
(0).toFixed.apply(Math.PI, [2]);
({}).hasOwnProperty.call(d, "foo");
/t/.test.call(/foo/, "bar");
/./.test.call(/foo/, "bar");
"".indexOf.call(e, "bar");
"#,
            r#"
Array.prototype.splice.apply(a, [
  1,
  2,
  b,
  c
]);
Function.prototype.call.apply(console.log, console, ["foo"]), Function.prototype.call.apply(console.log, console, ["foo"]);
Number.prototype.toFixed.call(Math.PI, 2);
Number.prototype.toFixed.apply(Math.PI, [2]);
Object.prototype.hasOwnProperty.call(d, "foo");
RegExp.prototype.test.call(/foo/, "bar");
RegExp.prototype.test.call(/foo/, "bar");
String.prototype.indexOf.call(e, "bar");
"#,
        );
    }

    #[test]
    fn leaves_non_matching_member_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
[1].splice.apply(a, []);
[].notARealArrayMethod.call(a);
0 .notARealNumberMethod.call(a);
({ foo: 1 }).hasOwnProperty.call(a, "foo");
/t/.notARealRegExpMethod.call(/foo/, "bar");
"foo".indexOf.call(e, "bar");
"".indexOf.bind(e, "bar");
"#,
            r#"
[1].splice.apply(a, []);
[].notARealArrayMethod.call(a);
0 .notARealNumberMethod.call(a);
({ foo: 1 }).hasOwnProperty.call(a, "foo");
/t/.notARealRegExpMethod.call(/foo/, "bar");
"foo".indexOf.call(e, "bar");
"".indexOf.bind(e, "bar");
"#,
        );
    }
}
