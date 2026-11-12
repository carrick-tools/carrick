/**
 * v2 check core — pure-unit tests (no pnpm, no tsc).
 *
 * Pins the deterministic pieces the byte-stable end-to-end runner depends on:
 * the (protocol, type_kind) direction table (incl. the confirmed HTTP
 * request-body inversion), stable pair IDs, the four-bucket classifier
 * precedence, the diagnostic scrub, and semver-dedupe override computation.
 */
export {};
