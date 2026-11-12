/**
 * carrick#1967, cause B: a list whose element has no symbol is still a list.
 *
 * A handler sends `rows.map(...)` building object literals, or a list typed by
 * an alias the compiler prints structurally. The model names the element
 * (`ItemDto`), bare by schema contract, so the use site's array-ness reaches
 * the capture only through the inference's `array_depth`. The inferrer used to
 * report that depth only beside a primary symbol, so a structural element
 * carried none, and the Rust depth join (`apply_inferred_array_depth`,
 * services/type_sidecar.rs) had nothing to copy onto the model's symbol.
 *
 * Pinned here: the depth is reported for a structural element at a send and at
 * a consumer's call result, and a named element keeps its symbol and depth.
 */
export {};
