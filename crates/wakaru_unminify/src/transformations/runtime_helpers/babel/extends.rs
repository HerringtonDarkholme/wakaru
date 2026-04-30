use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

use super::_spread;

const MODULE_NAME: &str = "@babel/runtime/helpers/extends";
const MODULE_ESM_NAME: &str = "@babel/runtime/helpers/esm/extends";

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    _spread::transform_ast(source, &[MODULE_NAME, MODULE_ESM_NAME])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn restores_cjs_extends_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
var _extends = require("@babel/runtime/helpers/extends");

a = _extends({}, y);
b = _extends.default({}, y);
c = (0, _extends)({}, y);
d = (0, _extends.default)({}, y);
"#,
            "
a = { ...y };
b = { ...y };
c = { ...y };
d = { ...y };
",
        );
    }

    #[test]
    fn restores_esm_extends_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
import _extends from "@babel/runtime/helpers/esm/extends";

a = _extends({}, y);
b = _extends.default({}, y);
c = (0, _extends)({}, y);
d = (0, _extends.default)({}, y);
"#,
            "
a = { ...y };
b = { ...y };
c = { ...y };
d = { ...y };
",
        );
    }

    #[test]
    fn flattens_object_arguments_and_nested_helper_calls() {
        define_ast_inline_test(transform_ast)(
            r#"
import _extends from "@babel/runtime/helpers/esm/extends";

a = _extends({ x }, y);
b = _extends({ x: z }, { y: "bar" });
c = _extends({ x }, { y: _extends({}, z) });
d = _extends(_extends(_extends({ a }, b), {}, { c }, d), {}, { e });
"#,
            r#"
a = {
  x,
  ...y
};
b = {
  x: z,
  y: "bar"
};
c = {
  x,
  y: { ...z }
};
d = {
  a,
  ...b,
  c,
  ...d,
  e
};
"#,
        );
    }

    #[test]
    fn keeps_helper_declaration_when_unprocessed_references_remain() {
        define_ast_inline_test(transform_ast)(
            r#"
var _extends = require("@babel/runtime/helpers/extends");

a = _extends({}, y);
console.log(_extends);
"#,
            r#"
var _extends = require("@babel/runtime/helpers/extends");
a = { ...y };
console.log(_extends);
"#,
        );
    }
}
