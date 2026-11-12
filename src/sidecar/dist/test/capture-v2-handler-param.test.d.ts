/**
 * carrick#498: a subscriber anchor must capture the payload its handler
 * RECEIVES, not the type of the registration call it sits on.
 *
 * Pre-fix, the subscriber side of a pub/sub pair reached capture as a
 * LINE-ONLY infer anchor (its collector deliberately sends no expression
 * text). `locateNode`'s line fallback then resolved the first expression on
 * that line, which is the enclosing `client.subscribe(...)` CALL — so the
 * alias captured that call's return type. With a `void`-returning subscribe
 * the surface line read `void`; with one returning a subscription handle it
 * read that handle. Both self-check clean, so nothing downstream caught it and
 * every correctly-typed publisher/subscriber pair read incompatible.
 *
 * The fix carries the `function_param` locator through to capture as
 * `param_name` and resolves the handler's parameter structurally: the last
 * function-typed argument of the call on the anchor's line (or a function
 * declared within two lines of it), then the parameter by name, by whole
 * destructured binding pattern, or by binding element. No method name,
 * library, or topic string is consulted anywhere.
 *
 * Fixtures are synthetic and generically named.
 */
export {};
