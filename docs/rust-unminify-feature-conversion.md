# Rust Unminify Feature Conversion

This document records how to convert `packages/unminify` transformations from TypeScript to Rust.

The goal is practical parity: each Rust transformation should be traceable to the original TS file, run in the same registry position, and prove the same behavior with focused tests. Rust code should use Oxc-native parsing, traversal, spans, semantic data, and codegen instead of recreating Babel parser or jscodeshift APIs.

## Ground Rules

- Keep the transformation registry mirrored in `crates/wakaru_unminify/src/transformations/mod.rs`.
- Do not split transformations into domain folders during the initial migration. One TS transform maps to one Rust file.
- Preserve duplicate registry passes, such as repeated `un-sequence-expression`.
- Treat Babel-generated output as input behavior that still needs support. Do not port Babel parser helpers.
- Prefer Oxc AST, spans, semantic analysis, and codegen over string matching.
- AST transforms must mutate the parsed Oxc AST in place. Do not return source text from AST transforms.
- String transforms may parse and reprint internally, but their public transform function returns source text.
- Add tests before broadening behavior. Keep fixtures readable and close to the TS inline test style.

## Conversion Workflow

1. Audit the TS feature.

Read the transform file, its tests, and any imported helpers. Record:

- matched AST patterns
- mutation behavior
- comments handling
- scope/reference assumptions
- rule options and pipeline params
- interactions with other registry passes

2. Classify the Rust implementation path.

Use one of these paths:

- `String transform`: the transform operates on source text and returns a new `String`. It reparses source directly if it needs an AST.
- `AST mutate`: the transform takes `&mut ParsedSourceFile`, mutates `program` in place, and returns `()`. The pipeline reparses once for a consecutive AST group and codegens once after the group.
- `Semantic transform`: the transform depends on bindings, references, import/export metadata, or unused declaration removal.
- `Pipeline/composite`: the feature composes other transforms or requires module metadata.
- `Deferred compatibility`: the TS feature depends on JS-only tools such as Lebab and needs a Rust replacement plan.

3. Port the smallest faithful behavior first.

Start with the behavior covered by existing TS tests. Avoid widening behavior during the first Rust port. If a Rust implementation intentionally covers less than TS, document that as a TODO in code or in the migration notes before wiring it into the default pipeline.

4. Add Rust inline tests.

Use `crates/wakaru_unminify/src/test_utils.rs`.

For string transforms:

```rust
let inline_test = define_inline_test(transform);

inline_test(
    "
input code
",
    "
expected code
",
);
```

For AST transforms:

```rust
let inline_test = define_ast_inline_test(transform_ast);

inline_test(
    "
input code
",
    "
expected code
",
);
```

The helpers normalize CRLF and trim input/output like `packages/test-utils/src/index.ts`. AST transform tests parse once, run the transform against `ParsedSourceFile`, and print with Oxc codegen.

5. Wire the transform.

Keep the module declaration and descriptor name aligned with the TS filename. The registry descriptor is the executable registration point:

```text
packages/unminify/src/transformations/un-use-strict.ts
crates/wakaru_unminify/src/transformations/un_use_strict.rs
TransformationDescriptor::ast("un-use-strict", un_use_strict::transform_ast)
```

Do not add id-based dispatch or hand-coded pass order in `pipeline.rs`. `run_default_transformations` iterates `default_transformation_registry()`. To migrate a pass, replace the registry row's pending function with the real transform function.

6. Verify JS/Rust behavior.

For every migrated transform:

- compare Rust fixtures with TS fixtures
- include comment-sensitive cases when the TS transform handles comments
- include negative cases where similar syntax must not change
- run Oxc parse validation on output
- run the CLI smoke path when the transform is wired into the pipeline

## Implementation Patterns

### String Transform

Use this for formatter/compatibility passes that naturally operate on source text, such as `oxfmt` and the future Oxc-native `lebab` replacement.

Expected shape:

- accept `&SourceFile`
- parse inside the transform if an AST is needed
- return transformed `String`
- let the pipeline treat the returned string as a new source boundary

Risks:

- every string transform invalidates any previous parsed AST
- codegen output can differ from Recast formatting
- comments may need explicit handling if the transform reparses and prints

### AST Mutate

Use this for transforms that restructure expressions or statements, such as `un-boolean`, `un-typeof`, `un-bracket-notation`, and many sequence-expression cases.

Expected shape:

- expose `pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()>`
- mutate `source.program` using Oxc visitors or direct AST operations
- allocate new nodes through Oxc allocation rules when inserting syntax
- return `Ok(())`, not source text
- let the pipeline codegen once after the consecutive AST group
- preserve behavior through tests, not byte-for-byte original formatting

Risks:

- Oxc arena lifetimes require transform-local allocation discipline
- comments may need explicit preservation
- codegen output will differ from Recast formatting and should be normalized through Oxc formatting support

### Semantic Transform

Use this for transforms that need binding/reference safety, such as import/export rename, smart inline, optional chaining cleanup, and unused declaration removal.

Expected shape:

- build Oxc semantic data after parsing
- use symbols/references rather than ad hoc identifier name scans
- keep module metadata from `wakaru_core::module` as explicit pipeline input

Risks:

- TS jscodeshift helpers sometimes rely on local AST ancestry instead of semantic correctness
- Babel output can contain helper shapes that look simple but require scope-safe rewrites

### Composite Transform

Use this when the TS registry entry is a grouping rule, for example `un-parameters`.

Expected shape:

- keep the public registry entry as one descriptor
- call the child Rust transforms internally in the same order as TS
- keep child files available for traceability if they exist as TS files

## Pipeline Execution Model

`run_default_transformations` is registry-driven:

- `String` registry entries run directly on the current `SourceFile` and return fresh source text.
- Consecutive `Ast` registry entries are grouped. The pipeline parses the current source once into `ParsedSourceFile`, runs every AST transform in that group against the same mutable Oxc `Program`, then prints once with Oxc codegen.
- Another `String` entry starts a new source boundary. Any following AST group reparses from that string output.
- The pipeline still parses the final output for validation.
- AST transforms that need synthetic trailing comments, currently `un-numeric-literal`, record them on `ParsedSourceFile`; the pipeline applies those replacements after all registered transformations so final formatting cannot erase them.
- Pending deferred string passes should not force a source boundary until they have a real Rust implementation. For now `lebab` remains in the mirrored registry as a no-op AST descriptor so earlier raw syntax metadata survives for later migrated passes.

This means AST transforms should not parse source themselves in the default path and should not recreate `SourceFile`. Standalone unit tests use `define_ast_inline_test` to provide the parsed source and codegen step.

## Review Checklist

Before marking a feature converted:

- The Rust file maps one-to-one to the TS transform file.
- The registry descriptor name still matches the TS rule name.
- Tests include the main TS fixture behavior.
- Tests include at least one negative case when false positives are plausible.
- Output is reparsed with Oxc somewhere in the verified path.
- Comments behavior is either matched or explicitly documented as a gap.
- Scope-sensitive rewrites use semantic data or are documented as intentionally limited.
- The CLI smoke test demonstrates the transform if it is wired into the default pipeline.

## Ordered Migration Log

This list records the audited migration order for the default `packages/unminify` registry. Keep the Rust registry mirrored in `crates/wakaru_unminify/src/transformations/mod.rs`, but port features in this order so small, verifiable passes land before transforms that need shared semantic infrastructure.

| Order | Transform | Path | Notes |
| ---: | --- | --- | --- |
| 0 | `oxfmt` | done | String transform replacement for JS formatter passes; already wired as `oxfmt` and `oxfmt-1`. |
| 1 | `un-use-strict` | done | AST mutate pass; already wired. |
| 2 | `un-esmodule-flag` | done | AST mutate pass removing CJS `__esModule` boilerplate; already wired. |
| 3 | `un-boolean` | done | AST mutate pass converting `!0` and `!1`; already wired. |
| 4 | `un-infinity` | done | AST mutate pass converting `1 / 0` and `-1 / 0`; already wired. |
| 5 | `un-typeof` | done | AST mutate pass expanding `typeof x < "u"` and mirrored comparisons; already wired. |
| 6 | `un-bracket-notation` | done | AST mutate pass simplifying string computed members to dot or numeric members; already wired. |
| 7 | `un-while-loop` | done | AST mutate pass converting `for (; test; )` and `for (;;)` to `while`; already wired. |
| 8 | `un-assignment-merging` | done | AST mutate pass splitting chained assignments into multiple statements when the final value is simple; already wired. |
| 9 | `un-variable-merging` | done | AST mutate pass splitting multi-declarator variable declarations and extracting unused `var` declarators from `for` initializers; wired. Parent-scope detection currently mirrors covered TS behavior through direct parent statement declarations and should move to Oxc semantic data before broadening. |
| 10 | `module-mapping` | done | AST mutate pass replacing mapped numeric/string `require` ids from pipeline params; wired. |
| 11 | `un-curly-braces` | done | AST mutate pass adding blocks around control-flow bodies, arrow expression bodies, and switch case consequents while preserving direct `var` declaration bodies; wired. |
| 12 | `un-return` | done | AST mutate pass simplifying direct final function/method returns: removes `return`, `return undefined`, and `return void 0`; converts `return void expr` to `expr;`; wired. |
| 13 | `un-numeric-literal` | done | AST mutate pass normalizing numeric literal spelling and preserving original raw value comments through the parsed-source synthetic trailing comment side channel; wired. |
| 14 | `un-template-literal` | done | AST mutate pass converting string `.concat` chains to real Oxc template literals; wired. |
| 15 | `un-type-constructor` | done | AST mutate pass restoring `Number`, `String`, and sparse `Array` constructor shapes; wired. |
| 16 | `un-builtin-prototype` | done | AST mutate pass restoring literal receiver `.call`/`.apply` chains to built-in prototype method calls; wired. |
| 17 | `un-flip-comparisons` | done | AST mutate pass reversing Yoda-style equality and relational comparisons when the left side is a simple constant/common value; wired. |
| 18 | `un-sequence-expression` | done | AST mutate pass splitting sequence expressions across statement-list contexts, including expression statements, returns, control-flow tests, variable declarations, and loop headers; wired for all duplicate registry occurrences. |
| 19 | `lebab` | skipped | Do not port for now. Keep the mirrored no-op AST registry entry so the default registry remains traceable without spending migration time here. |
| 20 | `un-export-rename` | done | Semantic transform merging top-level declaration aliases into named exports; uses Oxc symbol/reference IDs so recursive references are renamed while shadowed bindings are preserved; wired. |
| 21 | `un-import-rename` | done | Semantic transform renaming import aliases back to imported names, with sequential conflict suffixes and symbol/reference-safe use-site updates; wired. |
| 22 | `un-undefined` | done | Semantic transform converting numeric `void` expressions to `undefined` only when Oxc scope lookup confirms `undefined` is not declared in the current scope chain; wired. |
| 23 | Babel helper core | done | `array-like`, `array-without-holes`, `to-consumable-array`, `sliced-to-array`, `extends`, `object-spread`, and standard/loose `create-for-of` helper support are ported and wired through the Rust runtime helper composite, including loop-function body extraction. |
| 24 | `un-runtime-helper` | done | Runs the ported Babel helper core subset and annotates top-level helper functions from module metadata. Composite behavior was audited after `create-for-of` loop-function support landed; no additional wrapper changes were needed. |
| 25 | Babel interop helpers | done | Standalone AST helper transforms for `interopRequireDefault` and `interopRequireWildcard` are ported with default-member cleanup and namespace-import hint preservation. They remain intentionally outside `un-runtime-helper`, matching the TS structure, and are ready for `un-esm` integration. |
| 26 | `un-esm` | done | Runs Babel interop default/wildcard helpers, rebuilds existing imports plus top-level static `require` declarations/side-effect calls, dedupes named/bare imports, restores namespace imports from wildcard interop hints, converts `require("x").prop` destructuring with conflict-safe locals, annotates numeric `require(id)` as missing, restores `Promise.resolve().then(() => require("x"))` dynamic imports, converts the common top-level CJS export assignment forms, and supports TS-compatible hoist/fuzzy require mode through `TransformationParams::un_esm_hoist`. |
| 27 | `un-enum` | done | AST pass wired for the common TypeScript enum IIFE forms: declaration-paired IIFEs, mangled parameter names, string/numeric/heterogeneous members, invalid identifier keys, declaration merging with spread, terser `!function` calls, esbuild arrow IIFE variable initializers, SWC compressed assignment forms, source comment relocation, and reverse mapping comments. |
| 28 | `un-indirect-call` | done | Semantic transform for ESM default imports and top-level CJS `require` namespaces: indirect member calls become direct named calls, named imports/destructures are reused or inserted, local conflicts get aliases, shadowed locals are ignored via Oxc symbol IDs, and default imports are removed only when all references were converted. |
| 29 | `un-iife` | done | Semantic transform for top-level IIFEs: single-character params are renamed from longer identifier arguments using Oxc symbol/reference IDs, and literal arguments are moved into leading `const` declarations when `arguments` is not used. |
| 30 | `smart-rename` | done | Semantic pass is wired for object destructuring aliases in variable declarations and function parameters, plus React ecosystem heuristics for `createContext`, `useRef`, `useState`, `useReducer`, and `forwardRef`. It renames binding symbols and references, resolves conflicts with suffixes, and prefixes reserved/invalid binding names. |
| 31 | `smart-inline` | done | Semantic pass is wired for adjacent single-`const` temp variable inlining, chained temp assignments, browser-global alias inlining, single property-access path renaming, array destructuring reconstruction from consecutive numeric member reads, and object destructuring reconstruction from repeated property reads with generated-name conflict handling. It also retargets Oxc leading-comment attachments for merged declarations to preserve TS comment parity. |
| 32 | `un-optional-chaining` | partial | AST slice is wired for canonical Babel/SWC ternary member guards, computed members, optional direct calls, optional member calls, simple `.call`, `.apply`, and `.bind` optional calls, simple logical `||` and `&&` truthy guard chains, simple `delete` recovery, pure-getter identifier guards, container positions from the TS fixture, and removal of unused temp declarations. Remaining gaps: nested logical/delete decision-tree splitting, spread helper interaction, and semantic cleanup beyond simple temp declarations. |
| 33 | `un-nullish-coalescing` | partial | AST slice is wired for canonical Babel/SWC ternary guards, reversed operands, simple temp-assignment guards, logical `&&` leaf forms, and unused temp declaration removal. Remaining gaps: full decision-tree splitting, nested coalescing chains, negated forms, optional-chaining interaction beyond simple expressions, and broader temp cleanup. |
| 34 | `un-conditionals` | partial | AST slice is wired for expression-statement conditionals, simple `&&`/`||` guarded expressions with basic `!` negation folding, and nested ternary return rendering as early-return `if` statements. Remaining gaps: broader nested decision-tree rendering, switch reconstruction, logical trees beyond one level, and statement-container parity. |
| 35 | `un-default-parameter` | partial | AST slice restores body-start loose defaults, common Babel `arguments[n]` ternaries, and logical-only boolean default forms into positional/default formal parameters. Remaining gaps: broader scope/reference safety, exact TS generated-name parity, rest interaction, and full statement-container parity. |
| 36 | `un-parameter-rest` | partial | Semantic slice converts parameterless non-arrow functions that use implicit `arguments` into `...args`, including references from nested arrows, and recognizes Babel generated rest-copy loops with zero or existing-formal-parameter offsets. Remaining gap: exact upstream scope edge parity. |
| 37 | `un-parameters` | partial | Public composite registry entry is wired and currently runs the ported `un-default-parameter` and `un-parameter-rest` slices in TS order. |
| 38 | `un-argument-spread` | done | AST mutate pass converting safe `fn.apply(undefined/null, args)` and receiver-matched `obj.fn.apply(obj, args)` / computed-member variants to spread calls; wired. |
| 39 | `un-jsx` | partial | AST slice converts classic `createElement`/`React.createElement` calls for string/component/member tags, null/object/spread props, simple boolean/string/expression attributes, spread props, simple/nested children, and attribute-free classic fragments. Remaining gaps: automatic runtime, fragments with attributes beyond component fallback, dynamic component hoisting, component renaming, constant tag inlining, React/Object spread helper flattening, pure annotation cleanup, and full formatting parity. |
| 40 | `un-es6-class` | skipped | Do not port for now. Keep the mirrored no-op AST registry entry; class reconstruction is large, helper-sensitive, and not worth prioritizing in the Rust migration. |
| 41 | `un-async-await` | skipped | Do not port for now. Keep the mirrored no-op AST registry entry; reconstructing `__generator` / `__awaiter` is control-flow heavy and not worth prioritizing in the Rust migration. |
| 42 | `oxfmt-1` | done | Final formatting pass. |

`un-builtins` is intentionally not in this migration queue because it is not in the default TS registry and the TS implementation is a TODO stub. Keep its Rust skeleton for traceability, but do not wire it into the default pipeline until the feature exists upstream or a Rust-specific design is accepted.

## Active Partial-Pass TODOs

Work this list one pass at a time. Each item must either become `done` in the ordered log with focused tests, or be reclassified as `skipped` with the reason recorded.

| Order | Transform | Remaining Work |
| ---: | --- | --- |
| 32 | `un-optional-chaining` | Nested logical/delete decision-tree splitting, spread helper interaction, and semantic cleanup beyond simple temp declarations. |
| 33 | `un-nullish-coalescing` | Full decision-tree splitting, nested coalescing chains, negated forms, optional-chaining interaction beyond simple expressions, and broader temp cleanup. |
| 34 | `un-conditionals` | Broader nested decision-tree rendering, switch reconstruction, logical trees beyond one level, and statement-container parity. |
| 35 | `un-default-parameter` | Broader scope/reference safety, exact TS generated-name parity, rest interaction, and full statement-container parity. |
| 36 | `un-parameter-rest` / `un-parameters` | Exact upstream scope edge parity for rest conversion and composite verification after parameter slices are complete. |
| 39 | `un-jsx` | Automatic runtime, fragments with attributes beyond component fallback, dynamic component hoisting, component renaming, constant tag inlining, React/Object spread helper flattening, pure annotation cleanup, and full formatting parity. |

## Example: `un-use-strict`

`un-use-strict` is the first real Rust transform.

Classification: `AST mutate`.

Why it is a good first conversion:

- Oxc exposes directives directly through `Program.directives` and `FunctionBody.directives`.
- The TS behavior is small and covered by readable inline tests.
- It demonstrates Oxc parsing, AST mutation, registry-driven pipeline wiring, test helper usage, and CLI-visible output.

Current behavior:

- removes top-level and function-body `"use strict"` directives
- preserves non-directive string literals such as `return str === 'use strict'`
- mutates directive lists in place and validates output through the pipeline's Oxc parse step

Known limitation:

- TS explicitly merges comments attached to removed directives onto the next node. The Rust AST implementation removes directive nodes and Oxc codegen can drop comments that were attached only to removed directives. Add comment-retargeting support and comment attachment tests before relying on exact comment relocation parity.
