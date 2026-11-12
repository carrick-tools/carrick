/**
 * carrick#1877: what a call's result holds is what `await` yields for the
 * call's type, read off the language's await protocol and not off a name.
 *
 * A call typed `Promise<T>` or `PromiseLike<T>` has always published `T` and
 * anchored its row on `T`: the two were peeled by name. A class that extends
 * `Promise`, or one that only has a `then`, has a symbol of its own, so the
 * name test missed it: the row was anchored on the class, and a call
 * returned without `await` published the class itself, which is transport.
 *
 * The protocol reading was already there for a result carrier behind a
 * thenable of the source's own making (carrick#1376), and it did not reach
 * this case for two reasons. It was only asked on the way to a carrier, so
 * its answer was dropped where no carrier was behind the thenable. And it
 * read `then`'s first parameter as a callback as written, which the
 * platform's own `then` is not: a subclass of `Promise` inherits
 * `onfulfilled?: ((value: T) => ...) | null`, an optional, nullable callback.
 *
 * One reading now answers both the published text and the anchor, and it is
 * the one a carrier is found through. What it yields is `then`'s value, so a
 * subclass whose `then` yields something other than its type argument
 * publishes what it yields. A property called `then` that cannot be called
 * is no protocol, and the type is left as it is.
 */
export {};
