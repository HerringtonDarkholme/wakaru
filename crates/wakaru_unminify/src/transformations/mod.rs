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
use wakaru_core::source::{ParsedSourceFile, SourceFile};

pub const DEFAULT_TRANSFORMATION_REGISTRY: &[TransformationDescriptor] = &[
    TransformationDescriptor::string("oxfmt", oxfmt::transform),
    TransformationDescriptor::ast("module-mapping", pending_ast_transform),
    TransformationDescriptor::ast("un-curly-braces", pending_ast_transform),
    TransformationDescriptor::ast("un-sequence-expression", pending_ast_transform),
    TransformationDescriptor::ast("un-variable-merging", pending_ast_transform),
    TransformationDescriptor::ast("un-assignment-merging", pending_ast_transform),
    TransformationDescriptor::ast("un-runtime-helper", pending_ast_transform),
    TransformationDescriptor::ast("un-esm", pending_ast_transform),
    TransformationDescriptor::ast("un-enum", pending_ast_transform),
    TransformationDescriptor::string("lebab", pending_string_transform),
    TransformationDescriptor::ast("un-export-rename", pending_ast_transform),
    TransformationDescriptor::ast("un-use-strict", un_use_strict::transform_ast),
    TransformationDescriptor::ast("un-esmodule-flag", un_esmodule_flag::transform_ast),
    TransformationDescriptor::ast("un-boolean", pending_ast_transform),
    TransformationDescriptor::ast("un-undefined", pending_ast_transform),
    TransformationDescriptor::ast("un-infinity", pending_ast_transform),
    TransformationDescriptor::ast("un-typeof", pending_ast_transform),
    TransformationDescriptor::ast("un-numeric-literal", pending_ast_transform),
    TransformationDescriptor::ast("un-template-literal", pending_ast_transform),
    TransformationDescriptor::ast("un-bracket-notation", pending_ast_transform),
    TransformationDescriptor::ast("un-return", pending_ast_transform),
    TransformationDescriptor::ast("un-while-loop", pending_ast_transform),
    TransformationDescriptor::ast("un-indirect-call", pending_ast_transform),
    TransformationDescriptor::ast("un-type-constructor", pending_ast_transform),
    TransformationDescriptor::ast("un-builtin-prototype", pending_ast_transform),
    TransformationDescriptor::ast("un-sequence-expression", pending_ast_transform),
    TransformationDescriptor::ast("un-flip-comparisons", pending_ast_transform),
    TransformationDescriptor::ast("un-iife", pending_ast_transform),
    TransformationDescriptor::ast("un-import-rename", pending_ast_transform),
    TransformationDescriptor::ast("smart-inline", pending_ast_transform),
    TransformationDescriptor::ast("smart-rename", pending_ast_transform),
    TransformationDescriptor::ast("un-optional-chaining", pending_ast_transform),
    TransformationDescriptor::ast("un-nullish-coalescing", pending_ast_transform),
    TransformationDescriptor::ast("un-conditionals", pending_ast_transform),
    TransformationDescriptor::ast("un-sequence-expression", pending_ast_transform),
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

fn pending_string_transform(source: &SourceFile) -> Result<String> {
    Ok(source.code.clone())
}
