/**
 * carrick#2434: label structure v2 on `verify_library_claims`. Each field the
 * store's rebuild writes is checked against the package's own declarations,
 * one valid claim and one invalid claim per field:
 * - `method` on a route member (the member's own name is the method) and
 *   `ALL` on a route member that registers every method;
 * - `method_key` (where the method sits) and `method_default` (the method
 *   when the option is absent, or the one method a member sends);
 * - an op's `base`;
 * - `runs` on a maker (a hook runs the function it is given) and
 *   `runs: { bound: 'maker' }` on what that maker returns (`mutate` runs the
 *   function its maker was given);
 * - `yields` on a receive op (a server's connection handler is handed the
 *   server end of a socket);
 * - `server_framework`, read since this structure.
 *
 * Every package is invented and its declarations are hand-written from the
 * general shape (a client factory with a base URL option, verb members, a
 * request member whose method is an option, a router type, a hook that
 * returns `{ mutate }`, a socket class a server's connection event hands
 * over), never copied from a real package. Claims with none of these fields
 * are pinned unchanged by `library-claims.test.ts` and
 * `library-claims-http-parity.test.ts`.
 */
export {};
