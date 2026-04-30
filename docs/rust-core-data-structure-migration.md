# Rust Core Data Structure Migration

## JS Behavior Audit

The CLI exposes `all`, `unpacker`/`unpack`, and `unminify`, plus the default interactive command. The non-interactive commands require input globs, default output to `./out/`, split `all` output into `out/unpack` and `out/unminify`, reject existing output without `--force`, and run unpacker before unminify when both features are selected.

`packages/cli/src/unpacker.ts` reads one UTF-8 source file, calls `unpack(source)`, writes every extracted module with `moduleIdMapping[mod.id] ?? module-${id}.js`, and returns written files, modules, and the module-id mapping.

`packages/cli/src/unminify.worker.ts` reads one source file, calls `runDefaultTransformationRules(fileInfo, { moduleMeta, moduleMapping })`, and writes the returned `code`.

`packages/unpacker/src/Module.ts` defines the central module data shape: `id`, `isEntry`, `import`, `export`, `tags`, and `code`. `packages/unpacker/src/unpack.ts` parses the bundle, tries webpack then browserify, falls back to a single entry module, reparses extracted modules, scans imports/exports/runtime tags, and returns `{ modules, moduleIdMapping }`.

`packages/unminify/src/transformations/index.ts` defines one ordered registry. The Rust `wakaru_unminify` crate should mirror that order directly, including repeated `un-sequence-expression` passes. Formatter entries should use Oxc formatting names (`oxfmt` and `oxfmt-1`) rather than the JS formatter name.

`packages/shared/src/runner.ts` keeps either source text or a jscodeshift AST alive to avoid parse/print churn. Rust should eventually model this as pipeline state over Oxc AST/source/codegen, but the first prototype only validates parse and preserves source text.

## Rust Proposal

The first data model lives in `wakaru_core`:

- `SourceFile`: path plus source text, with Oxc parse validation through `parse_source`.
- `Diagnostic` and `WakaruError`: structured errors instead of direct stderr logging.
- `ImportInfo`: default, namespace, named, and bare imports.
- `Module`: `ModuleId`, entry flag, imports, exports, tags, and code.
- `ModuleMapping`: module id to emitted filename.
- `ModuleMetaMap`: module id to import/export/tag metadata.
- `TransformationDescriptor`: data-only rule descriptor used to mirror the JS registry without adding rule traits yet.
- `Timing` and `TimingStat`: Rust `Instant` counterpart to shared timing.

The first executable prototype wires the crates as follows:

- `wakaru_unpacker::unpack_source` validates input with Oxc and returns the JS fallback behavior: one entry module with id `0` and filename `entry.js`.
- `wakaru_unminify::pipeline::run_default_transformations` validates input with Oxc, records parse timing, and returns unchanged code.
- `wakaru_unminify::transformations::DEFAULT_TRANSFORMATION_REGISTRY` mirrors the JS transformation registry order as descriptors.
- `wakaru_cli` supports non-interactive `all`, `unpacker`/`unpack`, and `unminify` flow. The default root invocation behaves like `all` for prototype purposes instead of launching the JS interactive UI.

## JS/Rust Comparison

Matched now:

- CLI package boundaries: CLI calls unpacker, then unminify.
- Output layout for single feature vs `all`.
- Core bridge data: module, module mapping, module metadata, import/export/tag slots.
- Oxc replaces Babel parser and jscodeshift parser glue.
- Transformation registry order is visible in Rust and mirrors TS.
- The unpack fallback path produces a single entry module.

Intentionally deferred:

- Clack interactive UI, autocomplete, colors, spinners, worker pool, and perf JSON.
- Real webpack/browserify/JSONP extraction.
- Import/export/runtime metadata scans.
- Babel runtime helper rewrites.
- Oxc AST mutation, codegen, semantic scope/reference helpers, and real transformation execution.
- JS behavior where unminify worker swallows errors and still reports aggregate success.

Important difference to resolve later:

- JS has two related mappings: extractor `moduleIdMapping` for writing module files, and CLI-generated `moduleMapping` for unminify. Rust currently uses the canonical emitted filename mapping from `Module` until real extractors need short-name mappings.

## Success Criteria

This phase is successful when:

- The JS behavior audit is documented and tied to the migrated Rust data shapes.
- `wakaru_core` exposes the core data structures needed by unpacker, unminify, and CLI.
- Oxc parser validation is used in the prototype path.
- `wakaru_unminify` has a direct transformation registry mirror.
- The CLI can run an end-to-end prototype on a JS file.
- `cargo check --workspace` passes.
