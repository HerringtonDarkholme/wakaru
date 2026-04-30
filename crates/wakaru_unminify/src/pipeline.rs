use wakaru_core::diagnostics::{Diagnostic, Result};
use wakaru_core::module::{ModuleMapping, ModuleMetaMap};
use wakaru_core::source::{parse_source, SourceFile};
use wakaru_core::timing::Timing;

use crate::transformations::un_use_strict;

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
    let code = timing.measure(source.filename(), "un-use-strict", || {
        un_use_strict::transform(source)
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
