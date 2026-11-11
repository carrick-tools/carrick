# `socket-claims-xrepo`

Files `tests/socket_claims_parity_test.rs` adds to a copy of the socket repos of
`xrepo-corpus-1` (`payments-svc`, `web-frontend`) and `xrepo-corpus-3`
(`support-desk`, `storefront-web`). The corpora themselves are not edited: their
sources and `expected.json` are the answer key the test reads.

Per repo:

- `package-lock.json` (corpus 1 only): names the socket package at 4.7.5.
- `node_modules/socket.io{,-client}`: a hand-written declaration of the package,
  so the scan finds an installed dependency.
- `__llm__/library-claims/default.json`: the store's answer for the package
  (server side for `socket.io`, client side for `socket.io-client`).
- The repo's own `stubs.d.ts`, minus its ambient `declare module "socket.io"` /
  `"socket.io-client"` block.

The last one is the point of the overlay. An ambient module declaration stands
in for the package, so the fact layer has no package surface to read and states
no row; the corpora's socket rows then come only from the old socket.io rules.
With the declaration removed, as in a checkout with the package installed, the
fact layer states every `socket_events` row the answer key holds.
