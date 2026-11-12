/**
 * carrick#3313: a service with no tsconfig is typed with the loader's default
 * compiler options. Those defaults paired `module: ESNext` with
 * `moduleResolution: NodeNext`, a pair TypeScript 6 rejects (TS5110). Under
 * NodeNext every `import` in such a program reads as ESM, so a relative
 * import written without an extension (`./config`) or naming a directory
 * (`./lib`) never resolved (TS2835): every type read through it was `any`,
 * and every position-row link through it was a hole.
 *
 * The defaults now resolve the way a bundler-style project does (the same
 * `Bundler` resolution capture's synthesized defaults already use), so both
 * imports resolve, with or without `"type": "module"` in package.json.
 */
export {};
