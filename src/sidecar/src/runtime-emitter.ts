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

import * as path from 'node:path';
import { ts } from 'ts-morph';

/**
 * The runtime's own type packages. A type they declare (the runtime's event
 * emitter above all) is never part of another package's own declarations,
 * even when that package's maker returns it: a member only the runtime
 * declares is the runtime's, not the package's. When the named package is
 * one of these, it is its own.
 */
export const RUNTIME_TYPE_PACKAGES: ReadonlySet<string> = new Set([
  '@types/node',
  '@types/bun',
  'bun-types',
  '@types/deno',
]);

/**
 * The event names Node's runtime emits on, or treats specially for, every
 * `EventEmitter` (Node's events documentation: "Error events",
 * "Event: 'newListener'", "Event: 'removeListener'"). An emitter emits
 * `newListener` before it adds a listener and `removeListener` after it
 * removes one, and emitting `error` with no listener throws.
 */
export const RUNTIME_EMITTER_EVENTS: readonly string[] = ['error', 'newListener', 'removeListener'];

/** The file, relative to its package, that declares the runtime's emitter. */
const EMITTER_FILE = 'events.d.ts';

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
export function isRuntimeEmitterMember(
  property: ts.Symbol,
  packageOf: (file: ts.SourceFile) => string | undefined
): boolean {
  return (property.getDeclarations() ?? []).some(declaration => {
    if (!ts.isMethodDeclaration(declaration) && !ts.isMethodSignature(declaration)) return false;
    const owner = declaration.parent;
    if (!(ts.isClassLike(owner) || ts.isInterfaceDeclaration(owner)) || owner.name?.text !== 'EventEmitter') return false;
    if (!declaration.parameters.some(parameter => ts.isIdentifier(parameter.name) && parameter.name.text === 'eventName')) {
      return false;
    }
    const file = declaration.getSourceFile();
    const pkg = packageOf(file);
    if (pkg === undefined || !RUNTIME_TYPE_PACKAGES.has(pkg)) return false;
    const name = file.fileName.split(path.sep).join('/');
    const marker = `/node_modules/${pkg}/`;
    return name.slice(name.lastIndexOf(marker) + marker.length) === EMITTER_FILE;
  });
}
