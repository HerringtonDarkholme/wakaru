use wakaru_core::diagnostics::Result;
use wakaru_core::module::{module_mapping, Module, ModuleMapping};
use wakaru_core::source::{parse_source, SourceFile};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnpackResult {
    pub modules: Vec<Module>,
    pub module_id_mapping: ModuleMapping,
}

pub fn unpack_source(source: &SourceFile) -> Result<UnpackResult> {
    parse_source(source)?;

    let modules = vec![Module::new(0usize, source.code.clone(), true)];
    let module_id_mapping = module_mapping(&modules);

    Ok(UnpackResult {
        modules,
        module_id_mapping,
    })
}
