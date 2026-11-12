/**
 * Whether the type argument a retype fills can only SELECT (carrick#3158): in
 * the call's result it appears as a conditional's check or extends type, or
 * not at all. Such a call's result does not depend on the type stated, as
 * long as the stated type is not a union, so the retype's "the call's type
 * argument does not reach its result" can be answered before the rewrite for
 * any stated type that is a new object type.
 *
 * Reads only the consumer's code. Every rule fails closed: a shape this
 * file does not understand answers `false` and the call goes to the full
 * retype, which is unchanged.
 *
 *  C0  one signature, with type parameters and a written return type.
 *  C1  every type parameter after the first is closed: a written argument
 *      (explicit branch) or a default naming no type parameter. When the
 *      stated type breaks the first parameter's constraint, TypeScript fills
 *      unwritten arguments with their raw defaults, so a default that names
 *      a type parameter would change the result.
 *  C3  the call sits in no generic declaration: a producer type naming a
 *      type parameter in scope would defer the conditional.
 *  C2  the first parameter appears in the return type only in the check or
 *      extends type of a conditional that resolves (extends type without
 *      `infer`, only tracked type parameters named in either type).
 */
import { ts } from 'ts-morph';
import type { CallExpression } from 'ts-morph';
export declare function typeArgumentOnlySelects(call: CallExpression): boolean;
export declare function resolvedDeclaration(call: CallExpression): ts.SignatureDeclaration | undefined;
/**
 * C0: the declaration the call resolves to, when the callee has one call
 * signature (not a union), that signature has type parameters, and its
 * return type is written.
 */
export declare function singleSignatureDeclaration(call: CallExpression): (ts.SignatureDeclaration & {
    type: ts.TypeNode;
}) | undefined;
/** Whether `test` holds for any direct child (`forEachChild` stops only on a truthy return). */
export declare function anyChild(node: ts.Node, test: (child: ts.Node) => boolean): boolean;
/** Names no type parameter, no `this` and no `typeof`. */
export declare function isClosedType(node: ts.Node, checker: ts.TypeChecker): boolean;
export declare function referencedSymbol(node: ts.TypeReferenceNode, checker: ts.TypeChecker): ts.Symbol | undefined;
export declare function isTypeParameter(node: ts.TypeReferenceNode, checker: ts.TypeChecker): boolean;
export declare function holdsInfer(node: ts.Node): boolean;
