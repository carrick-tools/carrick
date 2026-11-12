/**
 * A payload locator that elides members with `...` names the whole payload.
 *
 * The model writes a long request payload as its opening and a `...`:
 * `body: { customerId: accountId, ... }`. That text is not in the source, and
 * not valid syntax, so the locator has no parse to read. The reverse rule (a
 * node whose text the locator contains) then bound the one member the text
 * quotes in full, `customerId: accountId`, which covers over half of the
 * text: the request was typed as that member's value (`string`) and a right
 * body read incompatible.
 *
 * An elision stands for the members it hides. The text before it is where the
 * source opens, so the locator binds the node whose text opens alike, or
 * nothing. A real spread (`...rest`) is source text and is untouched.
 */
export {};
