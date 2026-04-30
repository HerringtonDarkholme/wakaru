pub type ModuleSpecifier = String;
pub type ImportedName = String;
pub type LocalName = String;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportInfo {
    Default {
        name: LocalName,
        source: ModuleSpecifier,
    },
    Namespace {
        name: LocalName,
        source: ModuleSpecifier,
    },
    Named {
        name: ImportedName,
        local: LocalName,
        source: ModuleSpecifier,
    },
    Bare {
        source: ModuleSpecifier,
    },
}
