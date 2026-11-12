/**
 * Path-less mounts and pass-through members (K1a, the verifier half of the
 * koa-style mount spelling on carrick#2434).
 *
 * - `mounts: { arg, key?, handle: true }` is judged by the handler rule: the
 *   slot takes a declared function (a middleware), not the receiver itself.
 *   Without `handle` the instance rule is unchanged.
 * - A scope that binds (`mounts`, or `runs` with `yields`) may omit `name`.
 * - `passes: true` is a member whose value is its receiver, served at the
 *   receiver's own address: it stands alone, takes no string, and opens no
 *   `>scope:` receiver.
 * - `handle` on any slot but a scope's `mounts` is `claim_invalid`.
 *
 * The package is invented: a router whose `use` takes only middleware and
 * whose `routes()` hands back a middleware serving the router.
 */
export {};
