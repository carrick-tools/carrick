/**
 * #306: an inference that resolves to an array type must anchor on the ELEMENT
 * symbol and report the peeled depth. An array type's own symbol is the builtin
 * `Array` (filtered), so before this the anchor was silently dropped — and,
 * worse, an explicit anchor bundled for the same alias erased the use-site's
 * array-ness, scoring an array-vs-scalar mismatch as compatible.
 *
 * `array_depth` is what `resolve_all_types` (Rust) copies onto the matching
 * explicit `SymbolRequest`, so the bundler's existing array wrap (#248)
 * restores the `[]` levels on the producer comparand.
 */
export {};
