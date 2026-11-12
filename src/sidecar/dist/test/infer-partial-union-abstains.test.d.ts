/**
 * Regression for carrick#166 part B: a union whose members did not all resolve
 * must not answer with the ones that did.
 *
 * A handler that returns several branches resolves to a union. Each member is
 * unwrapped in turn, and a member that a rule verifies as transport but can
 * recover no payload from collapses to `unknown`. The join then dropped every
 * `unknown` — so a partial resolution could not read as a composite type — and
 * published whatever survived AS THE CONTRACT.
 *
 * Live shape (`POST /api/v1/authorization-code`): the awaited return is
 *
 *     TypedResponse<{ error: string }>
 *   | TypedResponse<CreateAuthorizationCodeResponse>
 *   | { status: number; body: string }
 *
 * The two wrapper members are intersections whose lib half IS the DOM
 * `Response`, which the extraction config verifies as machinery and finds no
 * payload in. Both collapsed, the bare early return survived, and the endpoint
 * published `{ status: number; body: string }` — a status envelope from one
 * guard branch, stated as the whole contract, with `is_explicit: false` and no
 * signal that anything was dropped.
 *
 * A contract known to be partial is a false match, and a false match is worse
 * than the `any` it replaced. So a partial union answers `unknown` and carries
 * the reason, naming how many branches were unread and what they were.
 *
 * Three negatives, because the rule must not swallow the cases it does not
 * describe: a union with no dropped members still prints every branch, a
 * single wrapper still prints its payload, and a union whose members ALL
 * collapse is unchanged — that verdict is the carrick#631 recovery's cue to
 * read the handler's returned arguments, and it must keep getting its turn.
 *
 * Reading the payload out of a wrapper that carries it in a member, and
 * unioning literal `new Response(...)` / `redirect(...)` branches, are the
 * other two halves of carrick#166 and are deliberately not done here.
 */
export {};
