/**
 * The verifier schema compiles on its own (carrick#2434, the seam pin for
 * carrick-cloud#2201).
 *
 * The companion cloud's library store vendors `src/validators.ts` byte for
 * byte, without `types.ts`, and type-checks it alone before its schema tests
 * run. Its only import that resolves there is `zod`; `./types.js` does not,
 * so anything in this file that leans on the message types (a compile-time
 * assertion over `SidecarRequest`, say) reads them as `any` and breaks the
 * cloud's lister pin. This compiles the file the way the cloud's gate does:
 * alone, strict, NodeNext, with that one unresolved import expected and
 * every other diagnostic a failure. Key parity between the schema and the
 * types lives in `request-keys-declared.test.ts`.
 */
export {};
