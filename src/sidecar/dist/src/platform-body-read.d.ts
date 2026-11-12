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
import { Node, ts, type CallExpression } from 'ts-morph';
/** The reads that take the whole body as JSON or as raw text (carrick#1842). */
export declare const WHOLE_BODY_READS: ReadonlySet<string>;
/** The JSON read alone. */
export declare const JSON_READ: ReadonlySet<string>;
/** The whole-body reads that are neither JSON nor text (carrick#2112). */
export declare const BINARY_BODY_READS: ReadonlySet<string>;
/** The read, and the body format a caller names, for raw text (carrick#1842). */
export declare const RAW_TEXT_FORMAT = "text";
/** Whether a declaring file is the runtime's: the default library or an installed package. */
export type RuntimeFile = (file: ts.SourceFile) => boolean;
/**
 * `call` is `<receiver>.<read>()` with no arguments, `read` is one of
 * `reads`, and the receiver is a platform value (`isPlatformValue`) or, with
 * `undeclared` set, a value nothing declares (`isUndeclaredValue`).
 */
export declare function isPlatformBodyRead(call: CallExpression, reads: ReadonlySet<string>, runtimeFile: RuntimeFile, undeclared: 'undeclared_reads' | 'platform_only'): boolean;
/** The value's type is `any` or `unknown`: nothing declares it. */
export declare function isUndeclaredValue(value: Node): boolean;
/**
 * The value's type, `null` and `undefined` aside, is a global the runtime
 * declares: every part of it is named by a symbol with a global declaration
 * in a runtime file. `any`, `unknown` and an anonymous type declare nothing,
 * so they are not.
 */
export declare function isPlatformValue(value: Node, runtimeFile: RuntimeFile): boolean;
