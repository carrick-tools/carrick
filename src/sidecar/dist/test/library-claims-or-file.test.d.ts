/**
 * A handler slot that takes the function or the path of a file exporting it
 * (QW-1, `handler: { arg, key?, or_file: true }`).
 *
 * - With `or_file`, the verifier drops the slot's file parts (string-like,
 *   or the platform `URL`) and judges the rest by the handler rule: at least
 *   one typed function must remain.
 * - A constructor whose second argument is options (`new Queue(name, opts)`)
 *   still fails: an options object is no file part.
 * - The lister marks such a parameter `function_or_file: true`, and writes
 *   nothing new on any other parameter.
 * - `or_file` on any slot but a make's or an op's `handler` is `claim_invalid`.
 *
 * The packages are invented.
 */
export {};
