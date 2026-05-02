use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

use super::un_default_parameter;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    un_default_parameter::transform_ast(source)
}
