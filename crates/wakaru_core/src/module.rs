use std::collections::BTreeMap;
use std::fmt::{self, Display};

use crate::ast::exports::ExportMap;
use crate::ast::imports::ImportInfo;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ModuleId(pub String);

impl ModuleId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for ModuleId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ModuleId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<usize> for ModuleId {
    fn from(value: usize) -> Self {
        Self::new(value.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Module {
    pub id: ModuleId,
    pub is_entry: bool,
    pub imports: Vec<ImportInfo>,
    pub exports: ExportMap,
    pub tags: BTreeMap<String, Vec<String>>,
    pub code: String,
}

impl Module {
    pub fn new(id: impl Into<ModuleId>, code: impl Into<String>, is_entry: bool) -> Self {
        Self {
            id: id.into(),
            is_entry,
            imports: Vec::new(),
            exports: BTreeMap::new(),
            tags: BTreeMap::new(),
            code: code.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleMeta {
    pub imports: Vec<ImportInfo>,
    pub exports: ExportMap,
    pub tags: BTreeMap<String, Vec<String>>,
}

impl From<&Module> for ModuleMeta {
    fn from(module: &Module) -> Self {
        Self {
            imports: module.imports.clone(),
            exports: module.exports.clone(),
            tags: module.tags.clone(),
        }
    }
}

pub type ModuleMetaMap = BTreeMap<ModuleId, ModuleMeta>;
pub type ModuleMapping = BTreeMap<ModuleId, String>;

pub fn module_file_name(module: &Module) -> String {
    if module.is_entry {
        if module.id.0 == "0" {
            "entry.js".to_string()
        } else {
            format!("entry-{}.js", module.id)
        }
    } else {
        format!("module-{}.js", module.id)
    }
}

pub fn module_meta_map(modules: &[Module]) -> ModuleMetaMap {
    modules
        .iter()
        .map(|module| (module.id.clone(), ModuleMeta::from(module)))
        .collect()
}

pub fn module_mapping(modules: &[Module]) -> ModuleMapping {
    modules
        .iter()
        .map(|module| (module.id.clone(), module_file_name(module)))
        .collect()
}
