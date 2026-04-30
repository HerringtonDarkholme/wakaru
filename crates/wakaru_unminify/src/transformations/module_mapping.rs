use oxc_ast::{
    ast::{Argument, CallExpression, Expression},
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_span::GetSpan;
use wakaru_core::diagnostics::Result;
use wakaru_core::module::{ModuleId, ModuleMapping};
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let mut mapper = ModuleMappingTransform {
        ast: AstBuilder::new(source.allocator),
        module_mapping: &source.params.module_mapping,
    };

    mapper.visit_program(&mut source.program);

    Ok(())
}

struct ModuleMappingTransform<'a, 'b> {
    ast: AstBuilder<'a>,
    module_mapping: &'b ModuleMapping,
}

impl<'a> VisitMut<'a> for ModuleMappingTransform<'a, '_> {
    fn visit_call_expression(&mut self, call: &mut CallExpression<'a>) {
        walk_mut::walk_call_expression(self, call);
        self.replace_require_argument(call);
    }
}

impl<'a> ModuleMappingTransform<'a, '_> {
    fn replace_require_argument(&self, call: &mut CallExpression<'a>) {
        if !is_require_call(call) || call.arguments.len() != 1 {
            return;
        }

        let Some(module_id) = module_id_from_argument(&call.arguments[0]) else {
            return;
        };

        let Some(replacement) = self.module_mapping.get(&module_id) else {
            return;
        };

        let span = call.arguments[0].span();
        let replacement = self.ast.allocator.alloc_str(replacement);
        call.arguments[0] =
            Argument::StringLiteral(self.ast.alloc_string_literal(span, replacement, None));
    }
}

fn is_require_call(call: &CallExpression) -> bool {
    matches!(&call.callee, Expression::Identifier(identifier) if identifier.name.as_str() == "require")
}

fn module_id_from_argument(argument: &Argument) -> Option<ModuleId> {
    match argument {
        Argument::StringLiteral(literal) => Some(ModuleId::new(literal.value.as_str())),
        Argument::NumericLiteral(literal) => Some(ModuleId::new(literal.value.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use oxc_allocator::Allocator;
    use oxc_ast::ast::Program;
    use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
    use wakaru_core::module::ModuleMapping;
    use wakaru_core::source::{parse_program, SourceFile, TransformationParams};

    use super::*;

    #[test]
    fn replaces_numeric_and_string_require_ids() {
        let mut module_mapping = ModuleMapping::new();
        module_mapping.insert(ModuleId::new("29"), "index.js".to_string());
        module_mapping.insert(ModuleId::new("foo"), "foo.js".to_string());

        let output = transform_with_module_mapping(
            "
const a = require(29);
const b = require(\"foo\");
",
            module_mapping,
        );

        assert_eq!(
            output,
            "const a = require(\"index.js\");\nconst b = require(\"foo.js\");\n"
        );
    }

    #[test]
    fn leaves_non_matching_require_calls_unchanged() {
        let mut module_mapping = ModuleMapping::new();
        module_mapping.insert(ModuleId::new("29"), "index.js".to_string());

        let output = transform_with_module_mapping(
            "
const a = require(30);
const b = require(29, extra);
const c = other(29);
const d = require(foo);
",
            module_mapping,
        );

        assert_eq!(
            output,
            "const a = require(30);\nconst b = require(29, extra);\nconst c = other(29);\nconst d = require(foo);\n"
        );
    }

    fn transform_with_module_mapping(input: &str, module_mapping: ModuleMapping) -> String {
        let source = SourceFile::from_parts(PathBuf::from("input.js"), input);
        let allocator = Allocator::default();
        let ret = parse_program(&allocator, &source).expect("input should parse");
        let params = TransformationParams {
            module_mapping,
            ..TransformationParams::default()
        };
        let mut parsed_source = ParsedSourceFile::new(&source, &allocator, ret.program, &params);

        transform_ast(&mut parsed_source).expect("transform should succeed");

        generate_code(&parsed_source.program)
    }

    fn generate_code(program: &Program) -> String {
        let options = CodegenOptions {
            indent_char: IndentChar::Space,
            indent_width: 2,
            ..CodegenOptions::default()
        };

        Codegen::new().with_options(options).build(program).code
    }
}
