use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

use super::{un_default_parameter, un_parameter_rest};

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    un_default_parameter::transform_ast(source)?;
    un_parameter_rest::transform_ast(source)
}
