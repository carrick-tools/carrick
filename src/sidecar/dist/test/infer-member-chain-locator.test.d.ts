/**
 * carrick#1162: a consumer response typed from a fragment of its call.
 *
 * A call written as a member chain breaks before the dot:
 *
 *     client
 *       .list({ source: 'system' })
 *       .then(({ tasks }) => …)
 *
 * The analyzer reports the call as `client.list({ source: 'system' })`. The text
 * locator collapsed whitespace to single spaces, so the source read
 * `client .list(…)` and never matched exactly; the substring fallback then bound
 * to the call's ARGUMENT, and the response was published as `{ source: string }`
 * (or a `string` from inside it) and judged against the producer's real body.
 *
 * A line break before a member access carries no meaning, so both sides of the
 * comparison drop whitespace around the dot. The response is then the value
 * the call site reads: the awaited result the `.then` callback receives.
 */
export {};
