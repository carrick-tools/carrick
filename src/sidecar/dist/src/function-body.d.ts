/**
 * The function body a call runs (carrick#3500). One answer for the helpers
 * that ask it: a declaration that has a body is that body; an overloaded
 * function is its one implementation (the signatures carry no body); a
 * variable or property initialised with an arrow or function expression is
 * that function, through the wrappers that leave a value as it is
 * (parentheses, `as`, `<T>`, `!`, `satisfies`).
 *
 * A name with two bodies, or with none, has no one body and answers
 * undefined.
 */
import { Node, type ArrowFunction, type FunctionDeclaration, type FunctionExpression, type MethodDeclaration } from 'ts-morph';
export type PlainFunctionBody = FunctionDeclaration | ArrowFunction | FunctionExpression;
export type FunctionBody = PlainFunctionBody | MethodDeclaration;
/** An expression without the wrappers that leave its value as it is. */
export declare function peelValueWrappers(node: Node): Node;
/**
 * The one function with a body among `declarations`, or undefined for none
 * or several. `methods: false` leaves method declarations out.
 */
export declare function functionBodyOf(declarations: readonly Node[], options: {
    methods: false;
}): PlainFunctionBody | undefined;
export declare function functionBodyOf(declarations: readonly Node[], options?: {
    methods?: boolean;
}): FunctionBody | undefined;
