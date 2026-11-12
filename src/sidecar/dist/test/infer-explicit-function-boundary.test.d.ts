/**
 * An annotation on a const does not describe a call inside a function in its
 * initializer.
 *
 * `const x: T = { handler: () => client.op(arg) }` annotates the object. The
 * call sits inside an arrow, so `T` is not the call's value: the call keeps
 * the type the compiler infers for it and is not explicit. A direct
 * `const y: T = client.op(arg)` still reads `T`.
 */
export {};
