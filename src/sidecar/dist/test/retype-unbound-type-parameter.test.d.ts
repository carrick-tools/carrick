/**
 * carrick#3897: a consumer call inside a generic wrapper whose result holds
 * the wrapper's own type parameter (`send<T>(...)` returned from
 * `readWidget<T>(): Promise<T>`) does not state the response. The call site
 * cannot bind that parameter: its real reads are at the wrapper's callers.
 * Stating the producer's type there makes the compiler compare a concrete
 * type with the open `T` ("'T' could be instantiated with an arbitrary
 * type"), which is no read of the response at all. The retype abstains.
 *
 * `client.ts`, `send.ts` and `inventory.ts` mirror the web side of the
 * `retype-reach-mono` fixture the issue names, inline, so the case is pinned
 * on its own. The rest are variants of the same shape and controls whose
 * result holds no type parameter of an enclosing declaration.
 */
export {};
