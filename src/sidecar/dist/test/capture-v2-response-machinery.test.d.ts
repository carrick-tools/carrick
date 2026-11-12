/**
 * carrick#371: a wrapped route-handler's literal return ENVELOPE must never be
 * emitted as a producer response contract.
 *
 * A Next.js-style producer `export const GET = withApiWrapper({ handler: () =>
 * ({ response: Response.json(data), error: undefined }) })` resolves its
 * handler return to `{ response: Response; error?: undefined } | { ...; error:
 * Error }` — framework transport machinery, not the JSON payload. Captured as
 * the response type, it manufactures a FALSE compat mismatch against the
 * consumer that anchors the real payload (`{ id; name }`). This is the
 * producer-side mirror of the consumer `machineryIndicators` guard.
 *
 * The fix is two fail-closed guards, both exercised here:
 *  1. The v1 inferrer (`buildFunctionReturnInferredType`) abstains when the
 *     resolved return IS or CONTAINS machinery — so no Literal anchor ever
 *     carries the envelope text (the primary production path).
 *  2. The capture Infer fallback (`resolveAnchor`) degrades to `unknown` when
 *     its resolved type IS or CONTAINS machinery — so the wrapper FUNCTION type
 *     `(req) => Promise<Response>` that the fallback resolves to never surfaces.
 *
 * Acceptable outcomes: the unwrapped payload, or `unknown`. Forbidden: any
 * concrete verdict off the machinery. Detection is structural + origin-gated;
 * no framework name is hardcoded.
 */
export {};
