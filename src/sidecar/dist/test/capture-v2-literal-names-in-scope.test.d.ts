/**
 * carrick#1774: a literal anchor's text names types as they read where it was
 * printed, and the surface entry reads it somewhere else.
 *
 * The v1 inferrer prints a type at its node, where a module's own aliases are
 * in scope, so a field typed by a string-union alias comes back as
 * `status?: InvoiceStatus | null`. The capture wrote that text into the
 * surface entry, where `InvoiceStatus` names nothing: the stub's member read
 * `any`, and the pair fell to unverifiable. The capture's own self-check said
 * `ok`, so this test reads the stub with the compiler instead.
 *
 * A name the text prints is now read in the anchor's source file. When it
 * means a type a repo module exports, the surface imports it from there; when
 * the source imports it from a package, the surface imports it from that
 * package (#1789).
 */
export {};
