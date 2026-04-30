use crate::diagnostics::Result;
use crate::source::{ParsedSourceFile, SourceFile};

pub type AstTransformationFn = for<'a> fn(&mut ParsedSourceFile<'a>) -> Result<()>;
pub type StringTransformationFn = fn(&SourceFile) -> Result<String>;

#[derive(Clone, Copy, Debug)]
pub enum TransformationKind {
    Ast(AstTransformationFn),
    String(StringTransformationFn),
    RuleSet(StringTransformationFn),
}

impl TransformationKind {
    pub fn run_ast<'a>(self, source: &mut ParsedSourceFile<'a>) -> Result<()> {
        match self {
            Self::Ast(transform) => transform(source),
            Self::String(_) | Self::RuleSet(_) => unreachable!("expected AST transformation"),
        }
    }

    pub fn run_string(self, source: &SourceFile) -> Result<String> {
        match self {
            Self::String(transform) | Self::RuleSet(transform) => transform(source),
            Self::Ast(_) => unreachable!("expected string transformation"),
        }
    }

    pub fn is_ast(self) -> bool {
        matches!(self, Self::Ast(_))
    }

    pub fn is_string(self) -> bool {
        match self {
            Self::Ast(_) => false,
            Self::String(_) | Self::RuleSet(_) => true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TransformationDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: TransformationKind,
    pub tags: &'static [&'static str],
}

impl TransformationDescriptor {
    pub const fn ast(id: &'static str, transform: AstTransformationFn) -> Self {
        Self {
            id,
            name: id,
            kind: TransformationKind::Ast(transform),
            tags: &[],
        }
    }

    pub const fn string(id: &'static str, transform: StringTransformationFn) -> Self {
        Self {
            id,
            name: id,
            kind: TransformationKind::String(transform),
            tags: &[],
        }
    }

    pub const fn rule_set(id: &'static str, transform: StringTransformationFn) -> Self {
        Self {
            id,
            name: id,
            kind: TransformationKind::RuleSet(transform),
            tags: &[],
        }
    }

    pub fn run_ast<'a>(self, source: &mut ParsedSourceFile<'a>) -> Result<()> {
        self.kind.run_ast(source)
    }

    pub fn run_string(self, source: &SourceFile) -> Result<String> {
        self.kind.run_string(source)
    }
}
