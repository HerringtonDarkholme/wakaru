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

use wakaru_core::rules::TransformationDescriptor;

pub const DEFAULT_TRANSFORMATION_REGISTRY: &[TransformationDescriptor] = &[
    TransformationDescriptor::string("oxfmt"),
    TransformationDescriptor::ast("module-mapping"),
    TransformationDescriptor::ast("un-curly-braces"),
    TransformationDescriptor::ast("un-sequence-expression"),
    TransformationDescriptor::ast("un-variable-merging"),
    TransformationDescriptor::ast("un-assignment-merging"),
    TransformationDescriptor::ast("un-runtime-helper"),
    TransformationDescriptor::ast("un-esm"),
    TransformationDescriptor::ast("un-enum"),
    TransformationDescriptor::string("lebab"),
    TransformationDescriptor::ast("un-export-rename"),
    TransformationDescriptor::ast("un-use-strict"),
    TransformationDescriptor::ast("un-esmodule-flag"),
    TransformationDescriptor::ast("un-boolean"),
    TransformationDescriptor::ast("un-undefined"),
    TransformationDescriptor::ast("un-infinity"),
    TransformationDescriptor::ast("un-typeof"),
    TransformationDescriptor::ast("un-numeric-literal"),
    TransformationDescriptor::ast("un-template-literal"),
    TransformationDescriptor::ast("un-bracket-notation"),
    TransformationDescriptor::ast("un-return"),
    TransformationDescriptor::ast("un-while-loop"),
    TransformationDescriptor::ast("un-indirect-call"),
    TransformationDescriptor::ast("un-type-constructor"),
    TransformationDescriptor::ast("un-builtin-prototype"),
    TransformationDescriptor::ast("un-sequence-expression"),
    TransformationDescriptor::ast("un-flip-comparisons"),
    TransformationDescriptor::ast("un-iife"),
    TransformationDescriptor::ast("un-import-rename"),
    TransformationDescriptor::ast("smart-inline"),
    TransformationDescriptor::ast("smart-rename"),
    TransformationDescriptor::ast("un-optional-chaining"),
    TransformationDescriptor::ast("un-nullish-coalescing"),
    TransformationDescriptor::ast("un-conditionals"),
    TransformationDescriptor::ast("un-sequence-expression"),
    TransformationDescriptor::ast("un-parameters"),
    TransformationDescriptor::ast("un-argument-spread"),
    TransformationDescriptor::ast("un-jsx"),
    TransformationDescriptor::ast("un-es6-class"),
    TransformationDescriptor::ast("un-async-await"),
    TransformationDescriptor::string("oxfmt-1"),
];

pub fn default_transformation_registry() -> &'static [TransformationDescriptor] {
    DEFAULT_TRANSFORMATION_REGISTRY
}
