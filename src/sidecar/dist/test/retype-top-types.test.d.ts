/**
 * carrick#1514: the retype check never reports a mismatch that an unresolved
 * type caused, and when a member of the producer's response is unresolved it
 * says which one instead.
 *
 * The retype states the producer's response in the CONSUMER's own program,
 * under the compiler options the sidecar reads for it. Without
 * `strictNullChecks`, `null` is assignable to every type, so the JSON wire
 * transform's `null extends { toJSON(): infer R }` test held and `R` came
 * back `unknown`. A producer member typed `null` in one branch of a union
 * then read as `unknown`, and a field the producer does return was flagged
 * as missing. That is a real defect for a consumer that is genuinely
 * non-strict.
 *
 * The consumer that exposed it is not one: it sets no `strict` and builds
 * with TypeScript 6, where `strict` defaults on, while the sidecar's bundled
 * TypeScript 5 reads the same config as non-strict (carrick#1519). Once that
 * is fixed, its row is judged strict and returns to a mismatch of its own
 * (`possibly 'null'`, from a discriminant published as `boolean`, #1516).
 *
 * Every other retype fixture, and the check workspace, is `strict: true`,
 * which is why none of them saw it. This one sets `strictNullChecks: false`
 * explicitly: the compiler's default for it changes between major versions.
 *
 * The client below is hand-written and synthetic.
 */
export {};
