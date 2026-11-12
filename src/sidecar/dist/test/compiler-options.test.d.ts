/**
 * carrick#2019: the sidecar runs on TypeScript 6, and every program it builds
 * carries `stableTypeOrdering` and TypeScript 5's value for each option whose
 * default 6 changed and the project leaves unset.
 *
 * - A type prints the same text in a process whatever that process checked
 *   first: a union's members, and the properties of an object a mapped type
 *   made, no longer come out in the order the checker met them.
 * - A project's tsconfig that leaves `strict`, `types`, `target`, `module`,
 *   `moduleResolution` or the interop options unset is read as TypeScript 5
 *   reads it: no strict null checks, every `@types/*` package, ES5, CommonJS,
 *   node10 resolution, no synthetic default imports.
 * - A tsconfig that sets an option TypeScript 6 deprecates (baseUrl,
 *   moduleResolution node, target es5) keeps loading, and so do the
 *   TypeScript 5 values the sidecar fills in: none raises an error.
 */
export {};
