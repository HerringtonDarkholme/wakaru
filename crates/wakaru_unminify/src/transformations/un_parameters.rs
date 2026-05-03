use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

use super::{un_default_parameter, un_parameter_rest};

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    un_default_parameter::transform_ast(source)?;
    un_parameter_rest::transform_ast(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn restores_default_and_rest_parameters_together() {
        define_ast_inline_test(transform_ast)(
            "
function fn(a1) {
  var a2 = arguments.length > 1 && arguments[1] !== undefined ? arguments[1] : 4;
  var _ref = arguments.length > 2 ? arguments[2] : undefined;
  a3 = _ref.a3;
  a4 = _ref.a4;
  for (var _len = arguments.length, rest = new Array(_len > 3 ? _len - 3 : 0), _key = 3; _key < _len; _key++) {
    rest[_key - 3] = arguments[_key];
  }
  return a1 + a2 + _ref + rest.length;
}
",
            "
function fn(a1, a2 = 4, _ref, ...rest) {
  a3 = _ref.a3;
  a4 = _ref.a4;
  return a1 + a2 + _ref + rest.length;
}
",
        );
    }
}
