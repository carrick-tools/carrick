/**
 * WP3 seam additions to the v2 capture bundle:
 *  - `array_depth` on symbol anchors (#248/#306): the anchor is the ELEMENT
 *    symbol, so the use-site's `[]` levels ride the anchor and the surface
 *    alias becomes `import('./m').Sym[]`.
 *  - `literal` anchors (the v1 inline-alias path): verbatim type text, with
 *    bare identifiers resolved through a sibling symbol anchor's module when
 *    one names the same symbol (so they don't dangle in the entry file).
 *
 * Drives `captureStub` in-process against a synthetic mini-repo.
 */
export {};
