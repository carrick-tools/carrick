/**
 * carrick#2474 R0: route fields on `verify_library_claims` (contract
 * amendment of 2026-10-22 on carrick#2434). Each field is checked against the
 * package's own declarations, one valid claim and one invalid claim per field:
 * - `mounts` on a scope: the instance handed there is served under the
 *   scope's name (`mount(prefix, child)`);
 * - `runs` with `yields` on a scope: the member runs a function and hands it
 *   a scoped instance (`register(fn, { prefix })`);
 * - `request` on a receive op: where the handler's request holds its path,
 *   URL and method.
 *
 * The package is invented and hand-written from the general shape of a
 * server package (a maker returning an app whose verb members take
 * `(path, handler)`, a sub-router maker, a mount member, a plugin member,
 * a request type with path, URL and method members), never copied from a
 * real one. Claims with none of these fields are pinned unchanged by
 * `library-claims.test.ts` and `library-claims-v2.test.ts`.
 */
export {};
