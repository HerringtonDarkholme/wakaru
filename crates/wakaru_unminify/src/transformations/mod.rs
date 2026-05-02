pub mod lebab;
pub mod module_mapping;
pub mod oxfmt;
pub mod runtime_helpers;
pub mod smart_inline;
pub mod smart_rename;
pub mod un_argument_spread;
pub mod un_assignment_merging;
pub mod un_async_await;
pub mod un_boolean;
pub mod un_bracket_notation;
pub mod un_builtin_prototype;
pub mod un_builtins;
pub mod un_conditionals;
pub mod un_curly_braces;
pub mod un_default_parameter;
pub mod un_enum;
pub mod un_es6_class;
pub mod un_esm;
pub mod un_esmodule_flag;
pub mod un_export_rename;
pub mod un_flip_comparisons;
pub mod un_iife;
pub mod un_import_rename;
pub mod un_indirect_call;
pub mod un_infinity;
pub mod un_jsx;
pub mod un_nullish_coalescing;
pub mod un_numeric_literal;
pub mod un_optional_chaining;
pub mod un_parameter_rest;
pub mod un_parameters;
pub mod un_return;
pub mod un_runtime_helper;
pub mod un_sequence_expression;
pub mod un_template_literal;
pub mod un_type_constructor;
pub mod un_typeof;
pub mod un_undefined;
pub mod un_use_strict;
pub mod un_variable_merging;
pub mod un_while_loop;

use wakaru_core::diagnostics::Result;
use wakaru_core::rules::TransformationDescriptor;
use wakaru_core::source::ParsedSourceFile;

pub const DEFAULT_TRANSFORMATION_REGISTRY: &[TransformationDescriptor] = &[
    TransformationDescriptor::string("oxfmt", oxfmt::transform_preserving_raw),
    TransformationDescriptor::ast("module-mapping", module_mapping::transform_ast),
    TransformationDescriptor::ast("un-curly-braces", un_curly_braces::transform_ast),
    TransformationDescriptor::ast(
        "un-sequence-expression",
        un_sequence_expression::transform_ast,
    ),
    TransformationDescriptor::ast("un-variable-merging", un_variable_merging::transform_ast),
    TransformationDescriptor::ast(
        "un-assignment-merging",
        un_assignment_merging::transform_ast,
    ),
    TransformationDescriptor::ast("un-runtime-helper", un_runtime_helper::transform_ast),
    TransformationDescriptor::ast("un-esm", un_esm::transform_ast),
    TransformationDescriptor::ast("un-enum", un_enum::transform_ast),
    TransformationDescriptor::ast("lebab", pending_ast_transform),
    TransformationDescriptor::ast("un-export-rename", un_export_rename::transform_ast),
    TransformationDescriptor::ast("un-use-strict", un_use_strict::transform_ast),
    TransformationDescriptor::ast("un-esmodule-flag", un_esmodule_flag::transform_ast),
    TransformationDescriptor::ast("un-boolean", un_boolean::transform_ast),
    TransformationDescriptor::ast("un-undefined", un_undefined::transform_ast),
    TransformationDescriptor::ast("un-infinity", un_infinity::transform_ast),
    TransformationDescriptor::ast("un-typeof", un_typeof::transform_ast),
    TransformationDescriptor::ast("un-numeric-literal", un_numeric_literal::transform_ast),
    TransformationDescriptor::ast("un-template-literal", un_template_literal::transform_ast),
    TransformationDescriptor::ast("un-bracket-notation", un_bracket_notation::transform_ast),
    TransformationDescriptor::ast("un-return", un_return::transform_ast),
    TransformationDescriptor::ast("un-while-loop", un_while_loop::transform_ast),
    TransformationDescriptor::ast("un-indirect-call", un_indirect_call::transform_ast),
    TransformationDescriptor::ast("un-type-constructor", un_type_constructor::transform_ast),
    TransformationDescriptor::ast("un-builtin-prototype", un_builtin_prototype::transform_ast),
    TransformationDescriptor::ast(
        "un-sequence-expression",
        un_sequence_expression::transform_ast,
    ),
    TransformationDescriptor::ast("un-flip-comparisons", un_flip_comparisons::transform_ast),
    TransformationDescriptor::ast("un-iife", un_iife::transform_ast),
    TransformationDescriptor::ast("un-import-rename", un_import_rename::transform_ast),
    TransformationDescriptor::ast("smart-inline", smart_inline::transform_ast),
    TransformationDescriptor::ast("smart-rename", smart_rename::transform_ast),
    TransformationDescriptor::ast("un-optional-chaining", un_optional_chaining::transform_ast),
    TransformationDescriptor::ast(
        "un-nullish-coalescing",
        un_nullish_coalescing::transform_ast,
    ),
    TransformationDescriptor::ast("un-conditionals", pending_ast_transform),
    TransformationDescriptor::ast(
        "un-sequence-expression",
        un_sequence_expression::transform_ast,
    ),
    TransformationDescriptor::ast("un-parameters", pending_ast_transform),
    TransformationDescriptor::ast("un-argument-spread", pending_ast_transform),
    TransformationDescriptor::ast("un-jsx", pending_ast_transform),
    TransformationDescriptor::ast("un-es6-class", pending_ast_transform),
    TransformationDescriptor::ast("un-async-await", pending_ast_transform),
    TransformationDescriptor::string("oxfmt-1", oxfmt::transform),
];

pub fn default_transformation_registry() -> &'static [TransformationDescriptor] {
    DEFAULT_TRANSFORMATION_REGISTRY
}

fn pending_ast_transform(_source: &mut ParsedSourceFile) -> Result<()> {
    Ok(())
}
