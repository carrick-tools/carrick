/**
 * A name slot typed by a type parameter with no constraint reads through its
 * default (T2b): `<T = string>(message: T)` takes a name string, `<T>(message: T)`
 * and `(message: unknown)` say nothing. Only the name, base and prefix check
 * reads the default. Every package below is invented.
 */
export {};
