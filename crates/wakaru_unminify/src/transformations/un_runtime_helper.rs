use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

use crate::transformations::runtime_helpers;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    runtime_helpers::transform_ast(source)
}
