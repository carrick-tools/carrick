/**
 * Deep (member-level) any/unknown detection — adversarial-review finding 1,
 * the CRITICAL false-compatible.
 *
 * The self-check's top-type gate was WHOLE-type only (`flags & Any|Unknown|
 * Never` on the alias root), and the check phase's probe gates are whole-type
 * too. A captured type carrying `any` at a member/element/type-arg position —
 * `{ orderId: string; metadata: any }`, `any[]`, `Promise<any>`,
 * `Record<string, any>` — recorded self_check 'ok', probed clean, and the
 * pair read COMPATIBLE against a counterparty that constrains that position
 * to a concrete shape. `any` is bidirectionally assignable: an arbitrary
 * shape reads compatible, which is the dangerous direction.
 *
 * Pins:
 *  1. capture: member/container any|unknown records decayed_internal with a
 *     any_provenance (first finding) and a reason;
 *  2. capture: fully-resolved types (including function-typed members and
 *     unions) are NOT over-demoted;
 *  3. check: a pair whose side carries a deep `any` verdicts
 *     gate_caught_baked_any (None downstream), a deep `unknown` verdicts
 *     unverifiable — never compatible.
 */
export {};
