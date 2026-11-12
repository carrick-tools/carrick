/**
 * Why a bare LLM symbol anchor cannot be trusted to carry array-ness.
 *
 * Live repro (carrick-demo trio, scanner v0.3.7): user-service's
 * `GET /api/users/:id/orders` answers `res.json(userOrders)` where
 * `userOrders` is `(await axios.get<Order[]>(...)).data.filter(...)`. The
 * producer is a correct `Order[]`; the consumer types a correct `Order[]`;
 * the scan reported a CAUTION type_mismatch — "Order missing properties from
 * Order[]: length, pop, push, concat, and 29 more".
 *
 * The two facts behind that, both pinned here:
 *
 *  1. On the bare checkout CI actually scans (#349) the client library is
 *     unresolved, so the payload decays to a BARE `any` and the
 *     `response_body` inference reports no `primary_type_symbol` and no
 *     `array_depth`. That is the only channel through which the use-site's
 *     `[]` can reach the capture — an LLM `primary_type_symbol` is a bare
 *     element identifier by schema contract.
 *
 *  2. A symbol anchor captured with no depth emits the ELEMENT as a confident
 *     surface line. Nothing downstream can tell that apart from a genuinely
 *     scalar contract.
 *
 * Together they mean array-ness was GUESSED, so the scanner-side fix
 * (`derive_capture_anchors`, engine/type_compat_v2.rs) declines to emit the
 * symbol anchor at all when the inference came back blind, leaving the alias
 * to its own infer anchor — which, as pinned below, captures `any` and
 * self-checks `decayed_internal`, routing the pair to unverifiable instead of
 * a false mismatch.
 */
export {};
