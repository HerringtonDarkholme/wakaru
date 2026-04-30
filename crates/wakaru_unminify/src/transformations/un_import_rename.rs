use std::collections::{HashMap, HashSet};

use oxc_ast::{
    ast::{
        BindingIdentifier, IdentifierReference, ImportDeclaration, ImportDeclarationSpecifier,
        ImportSpecifier, ModuleExportName,
    },
    AstBuilder,
};
use oxc_ast_visit::VisitMut;
use oxc_semantic::{Scoping, SemanticBuilder, SymbolId};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();

    let mut collector = ImportRenameCollector {
        ast: AstBuilder::new(source.allocator),
        occupied_names: collect_root_names(&scoping),
        renames: HashMap::new(),
    };

    collector.visit_program(&mut source.program);

    let mut renamer = ImportReferenceRenamer {
        ast: AstBuilder::new(source.allocator),
        scoping: &scoping,
        renames: collector.renames,
    };
    renamer.visit_program(&mut source.program);

    Ok(())
}

struct ImportRenameCollector<'a> {
    ast: AstBuilder<'a>,
    occupied_names: HashSet<String>,
    renames: HashMap<SymbolId, String>,
}

impl<'a> VisitMut<'a> for ImportRenameCollector<'a> {
    fn visit_import_declaration(&mut self, declaration: &mut ImportDeclaration<'a>) {
        let Some(specifiers) = &mut declaration.specifiers else {
            return;
        };

        for specifier in specifiers.iter_mut() {
            let ImportDeclarationSpecifier::ImportSpecifier(import_specifier) = specifier else {
                continue;
            };

            self.collect_import_specifier(import_specifier);
        }
    }
}

impl<'a> ImportRenameCollector<'a> {
    fn collect_import_specifier(&mut self, specifier: &mut ImportSpecifier<'a>) {
        let Some(imported_name) = imported_identifier_name(&specifier.imported) else {
            return;
        };

        let old_name = specifier.local.name.as_str().to_string();
        if imported_name == old_name {
            return;
        }

        let new_name = self.generate_unique_name(imported_name, &old_name);
        let Some(symbol_id) = specifier.local.symbol_id.get() else {
            return;
        };

        self.occupied_names.remove(&old_name);
        self.occupied_names.insert(new_name.clone());
        self.renames.insert(symbol_id, new_name.clone());
        specifier.local.name = self.ast.ident(&new_name);
    }

    fn generate_unique_name(&self, candidate: &str, old_name: &str) -> String {
        if candidate == old_name || !self.occupied_names.contains(candidate) {
            return candidate.to_string();
        }

        let mut index = 1;
        loop {
            let name = format!("{candidate}_{index}");
            if name == old_name || !self.occupied_names.contains(&name) {
                return name;
            }
            index += 1;
        }
    }
}

struct ImportReferenceRenamer<'a, 's> {
    ast: AstBuilder<'a>,
    scoping: &'s Scoping,
    renames: HashMap<SymbolId, String>,
}

impl<'a> VisitMut<'a> for ImportReferenceRenamer<'a, '_> {
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

fn collect_root_names(scoping: &Scoping) -> HashSet<String> {
    scoping
        .iter_bindings_in(scoping.root_scope_id())
        .map(|symbol_id| scoping.symbol_name(symbol_id).to_string())
        .collect()
}

fn imported_identifier_name<'a>(imported: &'a ModuleExportName<'a>) -> Option<&'a str> {
    match imported {
        ModuleExportName::IdentifierName(identifier) => Some(identifier.name.as_str()),
        ModuleExportName::IdentifierReference(identifier) => Some(identifier.name.as_str()),
        ModuleExportName::StringLiteral(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn renames_import_aliases_to_imported_names() {
        define_ast_inline_test(transform_ast)(
            "
import { foo as a, bar as b, code } from '_';

console.log(a, b, code);
",
            "
import { foo, bar, code } from \"_\";
console.log(foo, bar, code);
",
        );
    }

    #[test]
    fn resolves_naming_conflicts_sequentially() {
        define_ast_inline_test(transform_ast)(
            "
import defaultExport, { foo as a, bar as b, code } from 'A';
import { foo, bar as c } from 'B';

console.log(a, b, code, foo, c);
",
            "
import defaultExport, { foo as foo_1, bar, code } from \"A\";
import { foo, bar as bar_1 } from \"B\";
console.log(foo_1, bar, code, foo, bar_1);
",
        );
    }

    #[test]
    fn skips_existing_suffix_conflicts() {
        define_ast_inline_test(transform_ast)(
            "
import { foo as a, bar as b } from 'A';
import { foo, bar } from 'B';

const foo_1 = 'local';
console.log(a, b, foo, bar, foo_1);
",
            "
import { foo as foo_2, bar as bar_1 } from \"A\";
import { foo, bar } from \"B\";
const foo_1 = \"local\";
console.log(foo_2, bar_1, foo, bar, foo_1);
",
        );
    }

    #[test]
    fn does_not_touch_shadowed_references() {
        define_ast_inline_test(transform_ast)(
            "
import { foo as a } from 'A';

function test(a) {
  return a;
}

console.log(a);
",
            "
import { foo } from \"A\";
function test(a) {
  return a;
}
console.log(foo);
",
        );
    }

    #[test]
    fn leaves_string_named_imports_unchanged() {
        define_ast_inline_test(transform_ast)(
            "
import { \"foo-bar\" as a } from 'A';
console.log(a);
",
            "
import { \"foo-bar\" as a } from \"A\";
console.log(a);
",
        );
    }
}
