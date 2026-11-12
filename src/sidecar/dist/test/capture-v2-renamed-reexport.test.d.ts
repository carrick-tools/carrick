/**
 * A bundled package declares a type in a chunk file under a short internal
 * name and re-exports it from its public entry under another name
 * (`export { O as OmitKeys } from './chunk-hash.js'`). The v1 printer names the
 * type by the chunk's absolute path and internal name. The specifier rewrite
 * matches the entry's export by symbol identity, not by name, and writes the
 * name the entry exports.
 */
export {};
