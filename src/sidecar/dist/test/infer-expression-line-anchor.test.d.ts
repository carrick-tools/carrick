/**
 * Line-anchored expression resolution for pub/sub payload locators: the SAME
 * locator text (`payloadValue`) occurs at two publish sites with different
 * types (pubsub-handlers.ts lines 61 and 74, > the search window apart). The
 * pub/sub infer collector always supplies an `expression_line` — the model's
 * own line when reported, otherwise the operation's line — precisely so this
 * search stays anchored: each request must resolve its OWN site's type. This
 * pins the sidecar half of that contract.
 */
export {};
