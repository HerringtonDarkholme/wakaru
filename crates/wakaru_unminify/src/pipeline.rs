use wakaru_core::diagnostics::{Diagnostic, Result};
use wakaru_core::module::{ModuleMapping, ModuleMetaMap};
use wakaru_core::source::{parse_source, SourceFile};
use wakaru_core::timing::Timing;

use crate::transformations::{oxfmt, un_esmodule_flag, un_use_strict};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PipelineParams {
    pub module_mapping: ModuleMapping,
    pub module_meta: ModuleMetaMap,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransformationResult {
    pub path: String,
    pub code: String,
    pub diagnostics: Vec<Diagnostic>,
    pub timing: Timing,
}

pub fn run_default_transformations(
    source: &SourceFile,
    params: PipelineParams,
) -> Result<TransformationResult> {
    let mut timing = Timing::default();
    let _ = params;

    timing.measure(source.filename(), "oxc-parse", || parse_source(source))?;
    let formatted_code = timing.measure(source.filename(), "oxfmt", || oxfmt::transform(source))?;
    let formatted_source = SourceFile::from_parts(source.path.clone(), formatted_code);
    let code = timing.measure(source.filename(), "un-use-strict", || {
        un_use_strict::transform(&formatted_source)
    })?;
    let transformed_source = SourceFile::from_parts(source.path.clone(), code);
    let code = timing.measure(source.filename(), "un-esmodule-flag", || {
        un_esmodule_flag::transform(&transformed_source)
    })?;
    let transformed_source = SourceFile::from_parts(source.path.clone(), code);
    let code = timing.measure(source.filename(), "oxfmt-1", || {
        oxfmt::transform(&transformed_source)
    })?;
    let transformed_source = SourceFile::from_parts(source.path.clone(), code.clone());
    timing.measure(source.filename(), "oxc-parse-output", || {
        parse_source(&transformed_source)
    })?;

    Ok(TransformationResult {
        path: source.path.display().to_string(),
        code,
        diagnostics: Vec::new(),
        timing,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn formats_around_un_use_strict() {
        let source = SourceFile::from_parts(
            PathBuf::from("input.js"),
            "'use strict';function foo(){'use strict';return 1+2}",
        );

        let result = run_default_transformations(&source, PipelineParams::default())
            .expect("pipeline should succeed");

        assert_eq!(result.code, "function foo() {\n  return 1 + 2;\n}\n");
        assert_eq!(
            result
                .timing
                .stats()
                .iter()
                .map(|stat| stat.key.as_str())
                .collect::<Vec<_>>(),
            vec![
                "oxc-parse",
                "oxfmt",
                "un-use-strict",
                "un-esmodule-flag",
                "oxfmt-1",
                "oxc-parse-output"
            ]
        );
    }

    #[test]
    fn removes_esmodule_flag() {
        let source = SourceFile::from_parts(
            PathBuf::from("input.js"),
            "Object.defineProperty(exports,'__esModule',{value:!0});exports.foo=1;",
        );

        let result = run_default_transformations(&source, PipelineParams::default())
            .expect("pipeline should succeed");

        assert_eq!(result.code, "exports.foo = 1;\n");
    }
}
