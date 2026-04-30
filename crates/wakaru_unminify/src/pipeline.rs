use wakaru_core::diagnostics::{Diagnostic, Result};
use wakaru_core::module::{ModuleMapping, ModuleMetaMap};
use wakaru_core::source::{parse_source, SourceFile};
use wakaru_core::timing::Timing;

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

    Ok(TransformationResult {
        path: source.path.display().to_string(),
        code: source.code.clone(),
        diagnostics: Vec::new(),
        timing,
    })
}
