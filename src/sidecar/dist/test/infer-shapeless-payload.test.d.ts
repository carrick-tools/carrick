/**
 * carrick#2116: a narrowed `unknown` and `object` state no shape.
 *
 * A handler that returns a value it narrowed from `unknown` (`if (cached !=
 * null) return cached`) has the return type `{}`: the checker's own empty
 * object type, which also swallows a typed branch beside it. A request cast
 * `as object` is `object`. Both were published as written and judged as a
 * shape that lacks every field the other side names, so the pair read
 * incompatible on a type that states nothing.
 *
 * The printed text cannot decide this. An empty object a handler really sends
 * (`return {}`) and one its author declares (`(): {}`) print the same two
 * characters, and a consumer that expects fields from them is a true finding.
 * The source program tells them apart: the narrowed `unknown` is the one empty
 * object type with no symbol.
 *
 * Both places a payload type is read off the source program answer `unknown`
 * for it: the inferrer, and the capture's own locator read (which re-reads
 * the same locator whenever the inferrer's answer is not a decided one).
 */
export {};
