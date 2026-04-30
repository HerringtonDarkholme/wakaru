pub mod _spread;
pub mod array_like_to_array;
pub mod array_without_holes;
pub mod create_for_of_iterator_helper;
pub mod extends;
pub mod interop_require_default;
pub mod interop_require_wildcard;
pub mod object_spread;
pub mod sliced_to_array;
pub mod to_consumable_array;

use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    array_without_holes::transform_ast(source)
}
