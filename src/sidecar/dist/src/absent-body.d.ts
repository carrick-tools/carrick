/**
 * Whether a handler still works when no request body arrives (carrick#2814).
 *
 * A server library hands a handler `undefined` where a request carried no
 * body, whatever the handler's type says. A handler typed `{ limit?: number }`
 * that destructures the body, or reads `body.limit` without `?.`, throws on
 * that call, so a route that dereferences its body requires one even when its
 * type has no required member. These readers answer the question from the
 * handler's own source, by shape and with no library name consulted:
 *
 *  - every read of the body tolerates `undefined`: `body ?? {}`, `body || {}`,
 *    `body?.x`, a comparison, `typeof body`, `!body`, a condition, an object
 *    spread, or a binding default (`{ body = {} }`, `(payload = {}) =>`);
 *  - a name the body is bound to is followed to each of its own reads;
 *  - anything else (a destructure with no default, a plain member read, the
 *    body handed to a function) dereferences it, or may, so the answer is no.
 *
 * Conservative by construction: a read these rules do not recognise answers
 * no, and the check keeps the reading it had without the mark.
 */
import { type BindingElement, type ParameterDeclaration } from 'ts-morph';
import type { HandlerFunction } from './stated-params.js';
/**
 * A binding that receives the body, or a payload, tolerates `undefined`: it
 * has a default, or it binds a name whose every read tolerates it. A pattern
 * with no default destructures `undefined` and throws.
 */
export declare function bindingToleratesAbsence(binding: ParameterDeclaration | BindingElement): boolean;
/**
 * True when the handler reads its request body, and every read tolerates the
 * body being absent. False when it reads none: a body read some other way
 * (handed on with the request, read by a method) cannot be seen here.
 *
 * A body read is the `body` member of a value rooted at one of the handler's
 * parameters (`request.body`, `ctx.request.body`), or a binding that takes
 * `body` out of a parameter or out of such a value (`({ body }) =>`,
 * `const { body } = request`). A parameter the body is read through must not
 * be handed on whole (`service.create(request)`): what the callee reads off
 * it is not seen.
 */
export declare function handlerToleratesAbsentBody(handler: HandlerFunction): boolean;
