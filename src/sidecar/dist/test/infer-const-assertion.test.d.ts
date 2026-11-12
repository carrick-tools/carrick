/**
 * carrick#2590: a const assertion states no type.
 *
 * A table written as an array literal closed by `as const` holds calls in
 * its entries' functions. The type a read states was looked for on the
 * nearest `as` around it, and the nearest one was the const assertion around
 * the whole table: its type node prints as `const`, which was published as
 * the call's type. Nothing declares a type named `const`, so capture decayed
 * the alias to `unknown` and the side reached the index untyped.
 *
 * A const assertion narrows its operand's own type; it names none. The read
 * keeps the type the compiler infers for it, and a cast stated at the read
 * still wins.
 */
export {};
