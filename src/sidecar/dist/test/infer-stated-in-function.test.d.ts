/**
 * A value the compiler types `any` or `unknown` takes the type the function it
 * is made in states for it (carrick#2594).
 *
 * The value is a consumer call's result or a handler's body read. The source
 * states its type somewhere else in the same function:
 *
 *  - returned unchanged from a function with a declared return type;
 *  - handed in place to a schema (`Schema.parse(await call())`), also behind
 *    an empty default (`Schema.safeParse(request.body ?? {})`);
 *  - handed whole to a typed parameter, a callable branch dropped;
 *  - assigned to a separately declared name (`let body: T; body = read`),
 *    whose annotation states it, or which a schema or a cast then reads;
 *  - handed to a function that returns it unchanged (a narrowing cast).
 *
 * And it stays untyped where the function states nothing about the value
 * itself: a function that CONVERTS it into something new (its return type
 * describes what it built), a `Promise<void>` that discards it, a placeholder
 * cast, and two casts that disagree.
 *
 * Every locator here is a text locator, the shape the scanner sends from the
 * model's `expression_text`. No library, method or framework name is matched.
 */
export {};
