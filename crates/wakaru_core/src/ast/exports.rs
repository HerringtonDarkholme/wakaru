use std::collections::BTreeMap;

pub type ExportedName = String;
pub type LocalName = String;
pub type ExportMap = BTreeMap<ExportedName, LocalName>;
