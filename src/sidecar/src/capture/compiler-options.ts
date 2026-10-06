/**
 * The options every program the sidecar builds carries (carrick#2019).
 *
 * Every `ts.createProgram`, every ts-morph `Project` and the `tsconfig.json`
 * the check hands to `tsc` goes through `sidecarCompilerOptions`, whatever
 * its options came from: a project's tsconfig, a snapshot of one, a Deno
 * config, or the sidecar's own literal set.
 *
 * Two things are added:
 *
 * - `stableTypeOrdering`. Without it the compiler prints a union's members in
 *   the order the checker first met them, and an object made by a mapped type
 *   lists its properties in the order its keys were created, so the same type
 *   printed text that depended on what the process had read before. With it
 *   the checker orders types by their content, and one process prints the
 *   same text whatever it did first.
 * - TypeScript 5's effective value for every option whose default TypeScript 6
 *   changed, where the options leave it unset (`typescript5Defaults`). A
 *   project written against TypeScript 5 is read as its own compiler reads
 *   it, and as the sidecar read it before it moved to 6. A project that sets
 *   an option keeps its own value. Several of those values are deprecated in
 *   TypeScript 6 (target ES5, moduleResolution node10, esModuleInterop false),
 *   and so are options many projects still set themselves (baseUrl), so
 *   `ignoreDeprecations` is set too: a deprecated option still works in 6, and
 *   the error it would raise is not the project's.
 */
import ts from 'typescript';

/** Options the sidecar sets on every program, over anything else. */
const ALWAYS = {
  stableTypeOrdering: true,
  ignoreDeprecations: '6.0',
} as const;

/**
 * For each option whose default TypeScript 6 changed and `options` leaves
 * unset, the value TypeScript 5 (5.9, its last minor) computes for it from
 * the options that are set. Only unset options appear in the result.
 */
export function typescript5Defaults(options: ts.CompilerOptions): ts.CompilerOptions {
  const fill: ts.CompilerOptions = {};
  const { ModuleKind: M, ModuleResolutionKind: R, ScriptTarget: T } = ts;

  // 5.x ignores ES3 as it ignores no target: both take the default.
  const statedTarget = options.target === T.ES3 ? undefined : options.target;
  const target =
    statedTarget ??
    (options.module === M.Node16 || options.module === M.Node18
      ? T.ES2022
      : options.module === M.Node20
        ? T.ES2023
        : options.module === M.NodeNext
          ? T.ESNext
          : T.ES5);
  if (statedTarget === undefined) fill.target = target;

  const module = options.module ?? (target >= T.ES2015 ? M.ES2015 : M.CommonJS);
  if (options.module === undefined) fill.module = module;

  const moduleResolution =
    options.moduleResolution ??
    (module === M.CommonJS
      ? R.Node10
      : module === M.Node16 || module === M.Node18 || module === M.Node20
        ? R.Node16
        : module === M.NodeNext
          ? R.NodeNext
          : module === M.Preserve
            ? R.Bundler
            : R.Classic);
  if (options.moduleResolution === undefined) fill.moduleResolution = moduleResolution;

  const nodeFamily = module === M.Node16 || module === M.Node18 || module === M.Node20 || module === M.NodeNext;
  const esModuleInterop = options.esModuleInterop ?? (nodeFamily || module === M.Preserve);
  if (options.esModuleInterop === undefined) fill.esModuleInterop = esModuleInterop;
  if (options.allowSyntheticDefaultImports === undefined) {
    fill.allowSyntheticDefaultImports = esModuleInterop || module === M.System || moduleResolution === R.Bundler;
  }
  if (options.resolveJsonModule === undefined) {
    fill.resolveJsonModule = module === M.Node20 || module === M.NodeNext || moduleResolution === R.Bundler;
  }

  // Each strict flag a project leaves unset follows `strict`, which was off
  // unless set; `alwaysStrict` followed it too.
  if (options.strict === undefined) fill.strict = false;
  if (options.alwaysStrict === undefined) fill.alwaysStrict = options.strict ?? false;

  // Every package under each `typeRoots` directory (`@types/*`), not none.
  if (options.types === undefined) fill.types = ['*'];
  if (options.noUncheckedSideEffectImports === undefined) fill.noUncheckedSideEffectImports = false;
  if (options.libReplacement === undefined) fill.libReplacement = true;
  return fill;
}

/**
 * `options` with TypeScript 5's defaults filled in and the sidecar's own set.
 * Takes ts-morph's `CompilerOptions` as well as the compiler's: the two
 * declare the same option enums separately, and the values are the same.
 */
export function sidecarCompilerOptions<O extends object>(options: O): O {
  return { ...options, ...typescript5Defaults(options as ts.CompilerOptions), ...ALWAYS };
}
