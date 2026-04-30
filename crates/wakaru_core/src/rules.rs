#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransformationKind {
    Ast,
    String,
    RuleSet,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransformationDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: TransformationKind,
    pub tags: &'static [&'static str],
}

impl TransformationDescriptor {
    pub const fn ast(id: &'static str) -> Self {
        Self {
            id,
            name: id,
            kind: TransformationKind::Ast,
            tags: &[],
        }
    }

    pub const fn string(id: &'static str) -> Self {
        Self {
            id,
            name: id,
            kind: TransformationKind::String,
            tags: &[],
        }
    }
}
