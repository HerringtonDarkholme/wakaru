# Wakaru

[![codecov][CodecovBadge]][CodecovRepo]
[![Telegram-group](https://img.shields.io/badge/Telegram-group-blue)](https://t.me/wakarujs)


Wakaru is the Javascript decompiler for modern frontend. It brings back the original code from a bundled and transpiled source.

- 🔪📦 Unpacks bundled JavaScript into separated modules from [webpack][webpack] and [browserify][browserify].
- ⛏️📜 Unminifies transpiled code from [Terser][Terser], [Babel][Babel], [SWC][SWC], and [TypeScript][TypeScript].
- ✨📚 Detects and restores downgraded syntaxes (even with helpers!). See the [list](./packages//unminify/README.md#syntax-upgrade).
- 🧪🛡️ All cases are protected by tests. All code is written in TypeScript.

## Features

### Unminify

Converts transpiled code back to its readable form and restores downgraded syntaxes.

Supports the following transpilers:
  - Terser
  - Babel
  - SWC
  - TypeScript

[Read the documentation](./packages/unminify/README.md) for more information.

### Unpacker

Converts bundled JavaScript into separated modules

Supports the following bundlers:
  - webpack
  - browserify

## 🖥 Using the CLI

### Interactive mode

By default, the CLI will run in interactive mode and guide you through the process.\
You can also pass [options](#options) to skip some steps in the interactive mode.

```sh
npx @wakaru/cli
# or
pnpm dlx @wakaru/cli
```

### Options

Run `npx @wakaru/cli --help` to see the full list of options.

| Option          | Default | Description                             |
| --------------- | ------- | --------------------------------------- |
| `--output`      | `"out"` | Output directory                        |
| `--force`       | `false` | Force overwrite output directory        |
| `--concurrency` | `1`     | Specific the number of concurrent tasks |
| `--perf`        | `false` | Show performance metrics                |
| `--perf-output` |         | Performance metrics output directory    |

`--concurrency` can be used to speed up the process. But please aware that the process might OOM if the input file is too large.

### Non-interactive mode

If you want to run the CLI in non-interactive mode, you can specify the feature by passing the feature name as the first argument.

`unpacker` and `unminify` will run only the corresponding feature.\
`all` will run both `unpacker` and `unminify` sequentially.

```
npx @wakaru/cli all      <files...> [options]
npx @wakaru/cli unpacker <files...> [options]
npx @wakaru/cli unminify <files...> [options]
```

These options are **only** available in `all` mode.

| Option              | Default          | Description                        |
| ------------------- | ---------------- | ---------------------------------- |
| `--unpacker-output` | `"out/unpack"`   | Override unpacker output directory |
| `--unminify-output` | `"out/unminify"` | Override unminify output directory |

When running a single feature (either `unpacker` or `unminify`), the CLI will only uses the path specified in the `--output` option. This means that, unlike in the `all` mode where subdirectories (`out/unpack` and `out/unminify`) are automatically created within the output directory, in single feature mode, the output files are placed directly in the specified `--output` directory without any additional subdirectories.

## 📦 Using the API

```sh
npm install @wakaru/unpacker @wakaru/unminify
# or
pnpm install @wakaru/unpacker @wakaru/unminify
# or
yarn add @wakaru/unpacker @wakaru/unminify
```

<details>

<summary>Click to expand</summary>

### `@wakaru/unpacker`

```ts
import { unpack } from '@wakaru/unpacker';

const { modules, moduleIdMapping } = await unpack(sourceCode);
for (const mod of modules) {
  const filename = moduleIdMapping[mod.id] ?? `module-${mod.id}.js`;
  fs.writeFileSync(outputPath, mod.code, 'utf-8');
}
```

### `@wakaru/unminify`

```ts
import { runDefaultTransformationRules, runTransformationRules } from '@wakaru/unminify';

const file = {
  source: '...', // source code
  path: '...',   // path to the file, used for advanced usecases. Can be empty.
}
// This function will apply all rules that are enabled by default.
const { code } = await runDefaultTransformationRules(file);

// You can also specify the rules to apply. Order matters.
const rules = [
  'un-esm',
  ...
]
const { code } = await runTransformationRules(file, rules);
```

You can check all the rules at [/unminify/src/transformations/index.ts](https://github.com/pionxzh/wakaru/blob/main/packages/unminify/src/transformations/index.ts).

Please aware that this project is still in early development. The API might change in the future.

And the bundle size of these packages are huge. It might be reduced in the future. Use with caution on the browser.

</details>

## Legal Disclaimer

Usage of `wakaru` for attacking targets without prior mutual consent is illegal. It is the end user's responsibility to obey all applicable local, state and federal laws. Developers assume no liability and are not responsible for any misuse or damage caused by this program.

[TypeScript]: https://www.typescriptlang.org/
[browserify]: http://browserify.org/
[webpack]: https://webpack.js.org/
[Terser]: https://terser.org/
[Babel]: https://babeljs.io/
[SWC]: https://swc.rs/
[CodecovBadge]: https://img.shields.io/codecov/c/github/pionxzh/wakaru
[CodecovRepo]: https://codecov.io/gh/pionxzh/wakaru
## License

[MIT](./LICENSE)
