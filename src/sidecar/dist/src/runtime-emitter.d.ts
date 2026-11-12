/**
 * The event names Node's runtime gives every `EventEmitter` (carrick#3032).
 *
 * No declaration of the runtime's emitter states them: `error` is declared
 * nowhere in the runtime types package, and `newListener` and
 * `removeListener` are declared only from 25 on, through a conditional type
 * the lister does not read. A built-in of the language runtime is a fact of
 * the platform, so it is stated once here. The lister marks the members it
 * applies to (`runtime_events`) and the verifier accepts a `reserved` claim
 * for the names on those members, from this same table.
 */
import { ts } from 'ts-morph';
/**
 * The runtime's own type packages. A type they declare (the runtime's event
 * emitter above all) is never part of another package's own declarations,
 * even when that package's maker returns it: a member only the runtime
 * declares is the runtime's, not the package's. When the named package is
 * one of these, it is its own.
 */
export declare const RUNTIME_TYPE_PACKAGES: ReadonlySet<string>;
/**
 * The event names Node's runtime emits on, or treats specially for, every
 * `EventEmitter` (Node's events documentation: "Error events",
 * "Event: 'newListener'", "Event: 'removeListener'"). An emitter emits
 * `newListener` before it adds a listener and `removeListener` after it
 * removes one, and emitting `error` with no listener throws.
 */
export declare const RUNTIME_EMITTER_EVENTS: readonly string[];
/**
 * Whether `property` is a member of the runtime's own emitter: some
 * declaration of it sits in `events.d.ts` at the root of a runtime types
 * package, in a class or interface named `EventEmitter`, and takes a
 * parameter named `eventName`. A member another package declares (its own
 * `EventEmitter`, a class that redeclares `on`) never qualifies, nor does a
 * runtime member that takes no event name (`setMaxListeners`).
 *
 * `packageOf` names the package a source file is installed in.
 */
export declare function isRuntimeEmitterMember(property: ts.Symbol, packageOf: (file: ts.SourceFile) => string | undefined): boolean;
