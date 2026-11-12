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
/**
 * For each option whose default TypeScript 6 changed and `options` leaves
 * unset, the value TypeScript 5 (5.9, its last minor) computes for it from
 * the options that are set. Only unset options appear in the result.
 */
export declare function typescript5Defaults(options: ts.CompilerOptions): ts.CompilerOptions;
/**
 * `options` with TypeScript 5's defaults filled in and the sidecar's own set.
 * Takes ts-morph's `CompilerOptions` as well as the compiler's: the two
 * declare the same option enums separately, and the values are the same.
 */
export declare function sidecarCompilerOptions<O extends object>(options: O): O;
