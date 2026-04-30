# Rust Crate Structure Proposal

This document proposes the initial Rust workspace structure for migrating Wakaru from the current JavaScript/TypeScript monorepo to Rust.

The goal of the first phase is CLI-first parity for the core workflow:

1. Read bundled or minified JavaScript inputs.
2. Optionally unpack bundled code into modules.
3. Optionally run the unminify transformation pipeline.
4. Write output files and optional performance stats.

Oxc should be the JavaScript parser, AST, traversal, semantic, and codegen foundation. Babel parser and jscodeshift-specific helpers should not be ported directly. Babel output still needs to be handled because Wakaru transforms Babel-generated runtime helpers and downgraded syntax.

## Workspace Layout

Use a Cargo workspace at the repository root:

```text
Cargo.toml
crates/
  wakaru_cli/
  wakaru_core/
  wakaru_unpacker/
  wakaru_unminify/
```

Suggested dependency direction:

```text
wakaru_cli
  -> wakaru_unpacker
  -> wakaru_unminify
  -> wakaru_core

wakaru_unpacker
  -> wakaru_core

wakaru_unminify
  -> wakaru_core
```

`wakaru_core` should not depend on the higher-level crates.

## Package Mapping

```text
packages/cli        -> crates/wakaru_cli
packages/unpacker   -> crates/wakaru_unpacker
packages/unminify   -> crates/wakaru_unminify
packages/ast-utils  -> crates/wakaru_core, selectively
packages/shared     -> crates/wakaru_core, selectively
packages/ds         -> do not migrate as a standalone crate initially
packages/test-utils -> replace with Rust integration helpers as needed
packages/browserfs  -> do not migrate
```

## `wakaru_core`

`wakaru_core` is the Rust replacement for shared contracts, AST utilities, parsing, printing, diagnostics, timing, and rule execution.

It should not mirror jscodeshift APIs. Oxc's arena-allocated AST and Rust borrowing model need native Rust abstractions.

Suggested layout:

```text
crates/wakaru_core/src/
  lib.rs
  source.rs
  diagnostics.rs
  timing.rs
  module.rs
  rules.rs
  ast/
    mod.rs
    comments.rs
    edit.rs
    exports.rs
    identifiers.rs
    imports.rs
    matchers.rs
    scope.rs
```

Responsibilities:

- `source.rs`: parse source with Oxc, infer source type, print AST with Oxc codegen.
- `diagnostics.rs`: normalize parser, semantic, transform, and I/O errors.
- `timing.rs`: replacement for `packages/shared/src/timing.ts`.
- `module.rs`: shared `Module`, `ModuleMapping`, `ModuleMeta`, `ImportInfo`, and export metadata.
- `rules.rs`: Rust-native transformation rule trait and runner.
- `ast/*`: only the AST helpers needed by unpacker and unminify.

Suggested shared data types:

```rust
pub type ModuleId = String;
pub type ModuleMapping = BTreeMap<ModuleId, String>;
pub type ModuleMeta = BTreeMap<ModuleId, ModuleMetadata>;

pub struct Module {
    pub id: ModuleId,
    pub is_entry: bool,
    pub code: String,
    pub imports: Vec<ImportInfo>,
    pub exports: BTreeMap<String, String>,
    pub tags: BTreeMap<String, Vec<String>>,
}

pub struct ModuleMetadata {
    pub imports: Vec<ImportInfo>,
    pub exports: BTreeMap<String, String>,
    pub tags: BTreeMap<String, Vec<String>>,
}
```

Suggested rule abstraction:

```rust
pub trait Rule {
    fn id(&self) -> &'static str;
    fn run(&self, ctx: &mut RuleContext<'_>) -> Result<RuleOutcome>;
}
```

The current TypeScript runner has separate string and jscodeshift rules. In Rust, the runner can still support source-level rules where useful, but AST-backed rules should be the default.

## `wakaru_cli`

`wakaru_cli` should mirror `packages/cli` first, because the CLI is the product boundary that composes the core packages.

Suggested layout:

```text
crates/wakaru_cli/src/
  main.rs
  args.rs
  commands.rs
  interactive.rs
  path.rs
  output.rs
  perf.rs
```

Responsibilities:

- Parse commands and options.
- Resolve file globs.
- Validate inputs and outputs are inside the current working directory.
- Run `all`, `unpacker`, and `unminify` commands.
- Preserve output layout:
  - single-feature mode writes directly to `--output`.
  - `all` mode writes unpacked files to `out/unpack` and unminified files to `out/unminify`.
- Preserve `--force`, `--concurrency`, `--perf`, `--perf-output`, `--unpacker-output`, and `--unminify-output`.
- Use Rayon for parallel unminify work.

Suggested dependency replacements:

```text
yargs          -> clap
globby         -> globset + walkdir, or glob
fs-extra       -> std::fs plus small helpers
poolifier      -> rayon
picocolors     -> anstyle or owo-colors
@clack/prompts -> inquire or dialoguer
zod            -> typed Rust config and clap validation
```

Interactive mode can be migrated after non-interactive CLI parity.

## `wakaru_unpacker`

`wakaru_unpacker` should mirror `packages/unpacker`.

Suggested layout:

```text
crates/wakaru_unpacker/src/
  lib.rs
  unpack.rs
  extractors/
    mod.rs
    browserify.rs
    webpack/
      mod.rs
      jsonp.rs
      require_helpers.rs
      webpack4.rs
      webpack5.rs
  module_scan/
    mod.rs
    babel_runtime.rs
```

Public API:

```rust
pub fn unpack(source: &str, source_name: Option<&Path>) -> Result<UnpackResult>;

pub struct UnpackResult {
    pub modules: Vec<Module>,
    pub module_id_mapping: ModuleMapping,
}
```

Migration notes:

- Keep webpack 5, webpack 4, webpack JSONP, and browserify as separate extractors.
- Preserve fallback behavior: if no supported bundler is detected, return a single entry module.
- Preserve module metadata scanning:
  - imports
  - exports
  - Babel runtime helper tags
- Keep Babel runtime scanning because Babel output support is part of the product behavior, even though Babel parsing is not needed.

## `wakaru_unminify`

`wakaru_unminify` should mirror `packages/unminify` closely. Do not split transformations into domain folders during the initial port. A one-to-one layout makes parity tracking easier.

Suggested layout:

```text
crates/wakaru_unminify/src/
  lib.rs
  pipeline.rs
  transformations/
    mod.rs
    lebab.rs
    module_mapping.rs
    prettier.rs
    smart_inline.rs
    smart_rename.rs
    un_argument_spread.rs
    un_assignment_merging.rs
    un_async_await.rs
    un_boolean.rs
    un_bracket_notation.rs
    un_builtin_prototype.rs
    un_builtins.rs
    un_conditionals.rs
    un_curly_braces.rs
    un_default_parameter.rs
    un_enum.rs
    un_es6_class.rs
    un_esm.rs
    un_esmodule_flag.rs
    un_export_rename.rs
    un_flip_comparisons.rs
    un_iife.rs
    un_import_rename.rs
    un_indirect_call.rs
    un_infinity.rs
    un_jsx.rs
    un_nullish_coalescing.rs
    un_numeric_literal.rs
    un_optional_chaining.rs
    un_parameter_rest.rs
    un_parameters.rs
    un_return.rs
    un_runtime_helper.rs
    un_sequence_expression.rs
    un_template_literal.rs
    un_type_constructor.rs
    un_typeof.rs
    un_undefined.rs
    un_use_strict.rs
    un_variable_merging.rs
    un_while_loop.rs
    runtime_helpers/
      mod.rs
      babel/
        mod.rs
        _spread.rs
        array_like_to_array.rs
        array_without_holes.rs
        create_for_of_iterator_helper.rs
        extends.rs
        interop_require_default.rs
        interop_require_wildcard.rs
        object_spread.rs
        sliced_to_array.rs
        to_consumable_array.rs
  utils/
    mod.rs
    condition.rs
    decision_tree.rs
    import.rs
    is_helper_function_call.rs
```

`transformations/mod.rs` should mirror `packages/unminify/src/transformations/index.ts` and keep the current rule order visible in one place:

```rust
pub fn default_transformations() -> Vec<Box<dyn Rule>> {
    vec![
        box_rule(prettier::Prettier::new("prettier")),
        box_rule(module_mapping::ModuleMapping),
        box_rule(un_curly_braces::UnCurlyBraces),
        box_rule(un_sequence_expression::UnSequenceExpression),
        box_rule(un_variable_merging::UnVariableMerging),
        box_rule(un_assignment_merging::UnAssignmentMerging),

        box_rule(un_runtime_helper::UnRuntimeHelper),
        box_rule(un_esm::UnEsm),
        box_rule(un_enum::UnEnum),

        box_rule(lebab::Lebab),
        box_rule(un_export_rename::UnExportRename),
        box_rule(un_use_strict::UnUseStrict),
        box_rule(un_esmodule_flag::UnEsModuleFlag),
        box_rule(un_boolean::UnBoolean),
        box_rule(un_undefined::UnUndefined),
        box_rule(un_infinity::UnInfinity),
        box_rule(un_typeof::UnTypeof),
        box_rule(un_numeric_literal::UnNumericLiteral),
        box_rule(un_template_literal::UnTemplateLiteral),
        box_rule(un_bracket_notation::UnBracketNotation),
        box_rule(un_return::UnReturn),
        box_rule(un_while_loop::UnWhileLoop),
        box_rule(un_indirect_call::UnIndirectCall),
        box_rule(un_type_constructor::UnTypeConstructor),
        box_rule(un_builtin_prototype::UnBuiltinPrototype),
        box_rule(un_sequence_expression::UnSequenceExpression),
        box_rule(un_flip_comparisons::UnFlipComparisons),

        box_rule(un_iife::UnIife),
        box_rule(un_import_rename::UnImportRename),
        box_rule(smart_inline::SmartInline),
        box_rule(smart_rename::SmartRename),
        box_rule(un_optional_chaining::UnOptionalChaining),
        box_rule(un_nullish_coalescing::UnNullishCoalescing),
        box_rule(un_conditionals::UnConditionals),
        box_rule(un_sequence_expression::UnSequenceExpression),
        box_rule(un_parameters::UnParameters),
        box_rule(un_argument_spread::UnArgumentSpread),
        box_rule(un_jsx::UnJsx),
        box_rule(un_es6_class::UnEs6Class),
        box_rule(un_async_await::UnAsyncAwait),

        box_rule(prettier::Prettier::new("prettier-1")),
    ]
}
```

Notes:

- `prettier.rs` should become an Oxc codegen-backed formatting rule, not a dependency on Prettier.
- `lebab.rs` should probably start as a placeholder or targeted compatibility rule. Do not add a JavaScript runtime dependency for Lebab.
- `un_parameters.rs` should mirror the current merged rule and compose `un_default_parameter` plus `un_parameter_rest`.
- `runtime_helpers/babel/*` should be kept because handling Babel output is still required.

## Initial Migration Order

1. Create the Cargo workspace and empty crates.
2. Implement `wakaru_core::source` around Oxc parser and codegen.
3. Implement `wakaru_core::module`, `timing`, `diagnostics`, and `rules`.
4. Implement `wakaru_cli` non-interactive commands with stubs for unpack and unminify.
5. Port CLI path handling and output behavior.
6. Port `wakaru_unpacker` extractors and tests.
7. Port import/export/runtime-helper scanning.
8. Add `wakaru_unminify` transformation files as stubs in one-to-one layout.
9. Port transformations in current registry order.
10. Add interactive CLI after non-interactive parity.

## Triage

Migrate in the initial Rust port:

- CLI orchestration.
- Unpacker.
- Unminify pipeline and transformations.
- Shared timing, diagnostics, metadata, and rule concepts.
- AST helper concepts needed by unpacker and unminify.
- Babel output handling, especially runtime helper detection and rewriting.

Do not migrate initially:

- `packages/shared/src/jscodeshift.ts`
- `packages/shared/src/babylon.ts`
- `packages/shared/src/jscodeshiftRule.ts` as an API shape
- `packages/browserfs`
- `packages/test-utils` as a package
- `packages/ds` as a standalone crate
- old standalone `@wakaru/unpacker` and `@wakaru/unminify` CLIs
- Prettier and Lebab as runtime dependencies

## Dependency Starting Point

Initial Rust dependencies to consider:

```toml
[workspace.dependencies]
anyhow = "1"
thiserror = "2"
clap = { version = "4", features = ["derive"] }
rayon = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
walkdir = "2"
globset = "0.4"
regex = "1"
indexmap = "2"

oxc_allocator = "*"
oxc_ast = "*"
oxc_codegen = "*"
oxc_parser = "*"
oxc_semantic = "*"
oxc_span = "*"
oxc_traverse = "*"
```

Pin the Oxc crates to the same published version across the workspace once implementation starts.

