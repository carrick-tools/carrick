/**
 * carrick#695: classify a bare `x.verb("/lit", arg)` site by its RECEIVER.
 *
 * A member call whose first argument is a route-shaped literal carries no role
 * of its own: `app.get("/widgets", handler)` registers a route and
 * `api.get("/widgets", options)` requests one, and nothing in the call's shape
 * separates them (a last-argument function, an options bag and a verb name can
 * each equally belong to either). The ruling of 2026-09-05 is that no
 * structural rule may decide it.
 *
 * What CAN decide it is the receiver's type, which is a compiler question. This
 * kind answers exactly that and nothing more: what `x` is at the site, which
 * package DECLARES that type, and what the invoked member returns. It never
 * says "server" or "client" — the classification from those facts to a role
 * lives in the Rust driver, where the detected framework and data-fetcher
 * package lists are.
 *
 * The fixture installs two declaration-only packages, because that is the
 * condition the answer depends on: with no declarations on disk the receiver's
 * type is `any` and this kind must say so rather than guess.
 */
export {};
