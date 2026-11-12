/**
 * carrick#1659: `verify_library_claims` checks claims of the message roles
 * (`broker`, `in_process_bus`, `socket`) against the package's own
 * declarations, in the shape the contract pins on carrick#1564 (comment
 * 5937606126, sections 2 and 3). HTTP parity is pinned in
 * `library-claims-http-parity.test.ts`.
 *
 * Every package below is invented and its declarations are hand-written. The
 * three answered packages are a task SDK (`broker`: a definition maker whose
 * options carry the name and the handler, instance sends with the name bound
 * by the maker, export sends with the name at argument 0), a key-value
 * store's pub/sub client (`broker`, its client re-exported from a core
 * package) and a socket package (`socket`: a client maker and a server
 * class). The rest isolate one must-not-verify rule each, so deleting a rule
 * flips its own fixture.
 */
export {};
