# `socket-claims-xrepo`

Files `tests/socket_claims_parity_test.rs` adds to a copy of the socket repos of
`xrepo-corpus-1` (`payments-svc`, `web-frontend`) and `xrepo-corpus-3`
(`support-desk`, `storefront-web`): per repo, the store's answer for the socket
package in `__llm__/library-claims/default.json` (server side for `socket.io`,
client side for `socket.io-client`) and the framework-detect cassette.

The corpora are install-shaped, so the fact layer reads them as it reads an
installed checkout: each socket repo holds a hand-written stub package under
`node_modules/socket.io{,-client}` and a lockfile entry naming it, and its
`stubs.d.ts` carries no ambient `declare module "socket.io"` /
`"socket.io-client"` block. An ambient declaration stands in for the package,
so the fact layer has no package surface to read and states no row; the
corpus's socket rows would then come only from the old socket.io rules. The
corpora's sources and `expected.json` are the answer key the test reads.
