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
    array_like_to_array::transform_ast(source)?;
    array_without_holes::transform_ast(source)?;
    to_consumable_array::transform_ast(source)?;
    sliced_to_array::transform_ast(source)?;
    extends::transform_ast(source)?;
    object_spread::transform_ast(source)
}
