/**
 * #336: a `call_result` inference that resolves to an array type must anchor
 * on the ELEMENT symbol and report the peeled depth, exactly as the producer
 * paths do since #306. The live repro is `axios.get<Order[]>(...)`: the LLM
 * anchor is the bare element (`Order`), so the explicit bundle pre-claims the
 * consumer alias, and without a depth reported here `apply_inferred_array_depth`
 * (Rust) has nothing to copy onto the `SymbolRequest` and the bundler renders
 * `export interface <alias> {...}` with the `[]` gone. The producer keeps
 * `Order[]`, so every scan reports a false array-vs-scalar contract risk.
 */
export {};
