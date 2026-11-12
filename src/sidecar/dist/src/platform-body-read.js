/**
 * The platform's body reads (blueprint row N33).
 *
 * A zero-argument `json()`, `text()`, `arrayBuffer()`, `blob()`, `bytes()` or
 * `formData()` takes the whole body out of the platform's `Request` or
 * `Response`. The member's name picks which read it is; it never decides
 * that it is one. That is decided by the value the member is called on: its
 * type is a global the runtime declares, in the compiler's default library
 * (`lib.dom.d.ts`) or an installed runtime typings package that declares it
 * as a global (`declare global { interface Response ... }`). A repository's
 * own class with a `json()` member reads its own value, and a client that
 * copies the platform's shape from a package of its own is read through the
 * library store's label for its result, never by this name.
 *
 * A receiver nothing declares (`any` or `unknown`: `fetch` on a checkout whose
 * configuration loads neither the DOM library nor installed runtime typings)
 * has no declaration to decide by, so there the name is all there is and the
 * read is taken as it always was. A declaration that is not the runtime's
 * always decides against it.
 */
import { Node, ts } from 'ts-morph';
/** The reads that take the whole body as JSON or as raw text (carrick#1842). */
export const WHOLE_BODY_READS = new Set(['json', 'text']);
/** The JSON read alone. */
export const JSON_READ = new Set(['json']);
/** The whole-body reads that are neither JSON nor text (carrick#2112). */
export const BINARY_BODY_READS = new Set(['arrayBuffer', 'blob', 'bytes', 'formData']);
/** The read, and the body format a caller names, for raw text (carrick#1842). */
export const RAW_TEXT_FORMAT = 'text';
/**
 * `call` is `<receiver>.<read>()` with no arguments, `read` is one of
 * `reads`, and the receiver is a platform value (`isPlatformValue`) or, with
 * `undeclared` set, a value nothing declares (`isUndeclaredValue`).
 */
export function isPlatformBodyRead(call, reads, runtimeFile, undeclared) {
    const callee = call.getExpression();
    if (!Node.isPropertyAccessExpression(callee) || !reads.has(callee.getName()) || call.getArguments().length > 0) {
        return false;
    }
    const receiver = callee.getExpression();
    return (isPlatformValue(receiver, runtimeFile) ||
        (undeclared === 'undeclared_reads' && isUndeclaredValue(receiver)));
}
/** The value's type is `any` or `unknown`: nothing declares it. */
export function isUndeclaredValue(value) {
    try {
        const type = value.getType();
        return type.isAny() || type.isUnknown();
    }
    catch {
        return false;
    }
}
/**
 * The value's type, `null` and `undefined` aside, is a global the runtime
 * declares: every part of it is named by a symbol with a global declaration
 * in a runtime file. `any`, `unknown` and an anonymous type declare nothing,
 * so they are not.
 */
export function isPlatformValue(value, runtimeFile) {
    let type;
    try {
        type = value.getType().getNonNullableType();
    }
    catch {
        return false;
    }
    const parts = type.isUnion() ? type.getUnionTypes() : [type];
    return (parts.length > 0 &&
        parts.every((part) => (part.getSymbol()?.getDeclarations() ?? []).some((declaration) => isGlobalDeclaration(declaration.compilerNode) &&
            runtimeFile(declaration.getSourceFile().compilerNode))));
}
/** Declared in the global scope: a script file's top level, or a `declare global` block. */
function isGlobalDeclaration(node) {
    for (let at = node.parent; at; at = at.parent) {
        if (ts.isModuleDeclaration(at))
            return (at.flags & ts.NodeFlags.GlobalAugmentation) !== 0;
        if (ts.isSourceFile(at))
            return !ts.isExternalModule(at);
    }
    return false;
}
