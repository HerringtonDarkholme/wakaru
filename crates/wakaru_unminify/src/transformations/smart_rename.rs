use std::collections::{HashMap, HashSet};

use oxc_ast::{
    ast::{BindingIdentifier, BindingPattern, BindingProperty, IdentifierReference, PropertyKey},
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_semantic::{ScopeId, Scoping, SemanticBuilder, SymbolId};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();

    let mut collector = DestructuringRenameCollector {
        ast: AstBuilder::new(source.allocator),
        scoping: &scoping,
        generated_names: HashMap::new(),
        renames: HashMap::new(),
    };
    collector.visit_program(&mut source.program);

    let mut renamer = SymbolRenamer {
        ast: AstBuilder::new(source.allocator),
        scoping: &scoping,
        renames: collector.renames,
    };
    renamer.visit_program(&mut source.program);

    Ok(())
}

struct DestructuringRenameCollector<'a, 's> {
    ast: AstBuilder<'a>,
    scoping: &'s Scoping,
    generated_names: HashMap<ScopeId, HashSet<String>>,
    renames: HashMap<SymbolId, String>,
}

impl<'a> VisitMut<'a> for DestructuringRenameCollector<'a, '_> {
    fn visit_binding_property(&mut self, property: &mut BindingProperty<'a>) {
        self.rename_property(property);
        walk_mut::walk_binding_property(self, property);
    }
}

impl<'a> DestructuringRenameCollector<'a, '_> {
    fn rename_property(&mut self, property: &mut BindingProperty<'a>) {
        if property.computed {
            return;
        }

        let Some(key_name) = binding_property_key_name(&property.key) else {
            return;
        };

        if property.shorthand {
            return;
        }

        let Some(binding) = property_binding_identifier_mut(&mut property.value) else {
            return;
        };

        let old_name = binding.name.as_str().to_string();
        if key_name == old_name {
            property.shorthand = true;
            return;
        }

        let Some(symbol_id) = binding.symbol_id.get() else {
            return;
        };
        let scope_id = self.scoping.symbol_scope_id(symbol_id);
        let new_name = self.generate_target_name(scope_id, &key_name);

        self.generated_names
            .entry(scope_id)
            .or_default()
            .insert(new_name.clone());
        self.renames.insert(symbol_id, new_name.clone());
        binding.name = self.ast.ident(&new_name);
        property.shorthand = new_name == key_name;
    }

    fn generate_target_name(&self, scope_id: ScopeId, key_name: &str) -> String {
        let base = if is_valid_binding_identifier(key_name) {
            key_name.to_string()
        } else {
            format!("_{key_name}")
        };

        if !self.is_name_occupied(scope_id, &base) {
            return base;
        }

        let mut index = 1;
        loop {
            let candidate = format!("{base}_{index}");
            if !self.is_name_occupied(scope_id, &candidate) {
                return candidate;
            }
            index += 1;
        }
    }

    fn is_name_occupied(&self, mut scope_id: ScopeId, name: &str) -> bool {
        if self.scoping.find_binding(scope_id, name.into()).is_some() {
            return true;
        }

        loop {
            if self
                .generated_names
                .get(&scope_id)
                .is_some_and(|names| names.contains(name))
            {
                return true;
            }

            let Some(parent_scope_id) = self.scoping.scope_parent_id(scope_id) else {
                return false;
            };
            scope_id = parent_scope_id;
        }
    }
}

struct SymbolRenamer<'a, 's> {
    ast: AstBuilder<'a>,
    scoping: &'s Scoping,
    renames: HashMap<SymbolId, String>,
}

impl<'a> VisitMut<'a> for SymbolRenamer<'a, '_> {
    fn visit_binding_identifier(&mut self, identifier: &mut BindingIdentifier<'a>) {
        if let Some(new_name) = identifier
            .symbol_id
            .get()
            .and_then(|symbol_id| self.renames.get(&symbol_id))
        {
            identifier.name = self.ast.ident(new_name);
        }
    }

    fn visit_identifier_reference(&mut self, identifier: &mut IdentifierReference<'a>) {
        let Some(symbol_id) = identifier
            .reference_id
            .get()
            .and_then(|reference_id| self.scoping.get_reference(reference_id).symbol_id())
        else {
            return;
        };

        if let Some(new_name) = self.renames.get(&symbol_id) {
            identifier.name = self.ast.ident(new_name);
        }
    }
}

fn binding_property_key_name(key: &PropertyKey) -> Option<String> {
    match key {
        PropertyKey::StaticIdentifier(identifier) => Some(identifier.name.as_str().to_string()),
        PropertyKey::Identifier(identifier) => Some(identifier.name.as_str().to_string()),
        _ => None,
    }
}

fn property_binding_identifier_mut<'a, 'b>(
    pattern: &'b mut BindingPattern<'a>,
) -> Option<&'b mut BindingIdentifier<'a>> {
    match pattern {
        BindingPattern::BindingIdentifier(identifier) => Some(identifier),
        BindingPattern::AssignmentPattern(assignment) => match &mut assignment.left {
            BindingPattern::BindingIdentifier(identifier) => Some(identifier),
            _ => None,
        },
        _ => None,
    }
}

fn is_valid_binding_identifier(name: &str) -> bool {
    if is_reserved_identifier(name) {
        return false;
    }

    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    (first == '_' || first == '$' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphanumeric())
}

fn is_reserved_identifier(name: &str) -> bool {
    matches!(
        name,
        "arguments"
            | "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "null"
            | "return"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn renames_object_destructuring_aliases() {
        define_ast_inline_test(transform_ast)(
            "
const {
  gql: t,
  dispatchers: o,
  listener: i = noop,
  sameName: sameName
} = n;
o.delete(t, i);
",
            "
const { gql, dispatchers, listener = noop, sameName } = n;
dispatchers.delete(gql, listener);
",
        );
    }

    #[test]
    fn renames_object_destructuring_in_function_parameters() {
        define_ast_inline_test(transform_ast)(
            "
function foo({
  gql: t,
  dispatchers: o,
  listener: i
}) {
  o.delete(t, i);
}

const foo2 = ({
  gql: t,
  dispatchers: o,
  listener: i
}) => {
  t[o].delete(i);
}
",
            "
function foo({ gql, dispatchers, listener }) {
  dispatchers.delete(gql, listener);
}
const foo2 = ({ gql, dispatchers, listener }) => {
  gql[dispatchers].delete(listener);
};
",
        );
    }

    #[test]
    fn resolves_name_conflicts() {
        define_ast_inline_test(transform_ast)(
            "
const gql = 1;

function foo({
  gql: t,
  dispatchers: o,
  listener: i
}, {
  gql: a,
  dispatchers: b,
  listener: c
}) {
  o.delete(t, i, a, b, c);
}
",
            "
const gql = 1;
function foo({ gql: gql_1, dispatchers, listener }, { gql: gql_2, dispatchers: dispatchers_1, listener: listener_1 }) {
  dispatchers.delete(gql_1, listener, gql_2, dispatchers_1, listener_1);
}
",
        );
    }

    #[test]
    fn prefixes_reserved_binding_names() {
        define_ast_inline_test(transform_ast)(
            "
const {
  static: t,
  default: o,
} = n;
o.delete(t);
",
            "
const { static: _static, default: _default } = n;
_default.delete(_static);
",
        );
    }
}
