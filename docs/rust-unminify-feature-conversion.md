# Rust Unminify Feature Conversion

This document records how to convert `packages/unminify` transformations from TypeScript to Rust.

The goal is practical parity: each Rust transformation should be traceable to the original TS file, run in the same registry position, and prove the same behavior with focused tests. Rust code should use Oxc-native parsing, traversal, spans, semantic data, and codegen instead of recreating Babel parser or jscodeshift APIs.

## Ground Rules

- Keep the transformation registry mirrored in `crates/wakaru_unminify/src/transformations/mod.rs`.
- Do not split transformations into domain folders during the initial migration. One TS transform maps to one Rust file.
- Preserve duplicate registry passes, such as repeated `un-sequence-expression`.
- Treat Babel-generated output as input behavior that still needs support. Do not port Babel parser helpers.
- Prefer Oxc AST, spans, semantic analysis, and codegen over string matching.
- Use source-text edits only for small transforms where Oxc spans identify the exact syntax to remove or replace.
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

- `Span edit`: Oxc identifies exact source ranges and no reprint is needed.
- `AST mutate + codegen`: the transform changes tree shape, inserts nodes, or needs formatting.
- `Semantic transform`: the transform depends on bindings, references, import/export metadata, or unused declaration removal.
- `Pipeline/composite`: the feature composes other transforms or requires module metadata.
- `Deferred compatibility`: the TS feature depends on JS-only tools such as Lebab and needs a Rust replacement plan.

3. Port the smallest faithful behavior first.

Start with the behavior covered by existing TS tests. Avoid widening behavior during the first Rust port. If a Rust implementation intentionally covers less than TS, document that as a TODO in code or in the migration notes before wiring it into the default pipeline.

4. Add Rust inline tests.

Use `crates/wakaru_unminify/src/test_utils.rs`:

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

The helper normalizes CRLF and trims input/output like `packages/test-utils/src/index.ts`.

5. Wire the transform.

Keep the module declaration and descriptor name aligned with the TS filename:

```text
packages/unminify/src/transformations/un-use-strict.ts
crates/wakaru_unminify/src/transformations/un_use_strict.rs
TransformationDescriptor::ast("un-use-strict")
```

When a transform becomes executable, connect it in `pipeline.rs` at the matching registry position. The current prototype only runs `un-use-strict`; as the runner matures, execution should be driven by the mirrored registry rather than hand-coded calls.

6. Verify JS/Rust behavior.

For every migrated transform:

- compare Rust fixtures with TS fixtures
- include comment-sensitive cases when the TS transform handles comments
- include negative cases where similar syntax must not change
- run Oxc parse validation on output
- run the CLI smoke path when the transform is wired into the pipeline

## Implementation Patterns

### Span Edit

Use this for transforms like `un-use-strict`, where Oxc exposes directive spans and the output can preserve the surrounding source text.

Expected shape:

- parse with Oxc
- collect spans with an Oxc visitor
- sort ranges
- remove or replace ranges from source text
- parse transformed output with Oxc in the pipeline

Risks:

- comments can be dropped or moved differently from TS
- semicolon/newline handling can create accidental token joins
- overlapping ranges must be sorted and skipped safely

### AST Mutate + Codegen

Use this for transforms that restructure expressions or statements, such as `un-boolean`, `un-typeof`, `un-bracket-notation`, and many sequence-expression cases.

Expected shape:

- parse with Oxc
- mutate AST using Oxc allocation rules
- print with Oxc codegen
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

## Example: `un-use-strict`

`un-use-strict` is the first real Rust transform.

Classification: `Span edit`.

Why it is a good first conversion:

- Oxc exposes directives directly through `Program.directives` and `FunctionBody.directives`.
- The TS behavior is small and covered by readable inline tests.
- It demonstrates Oxc parsing, AST visiting, source editing, pipeline wiring, test helper usage, and CLI-visible output.

Current behavior:

- removes top-level and function-body `"use strict"` directives
- preserves non-directive string literals such as `return str === 'use strict'`
- validates output through the pipeline's Oxc parse step

Known limitation:

- TS explicitly merges comments attached to removed directives onto the next node. The Rust span-edit implementation preserves leading comments that are separate source lines, but it does not yet model attached AST comments. Add comment attachment tests before relying on exact comment relocation parity.
