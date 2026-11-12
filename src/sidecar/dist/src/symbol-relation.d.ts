/**
 * How a type the model named for a producer's response relates to the
 * compiler's type of what the handler sends (carrick#2111).
 *
 * The model names one symbol per row, and by schema it is a bare name. The
 * compiler reads the response expression itself. Where the two are the same
 * type the name is the better thing to publish; where they are not, the name
 * is wrong about the body: an element named for a list, a helper's input type
 * named for its result, a type named for the wrapper sent around it.
 *
 * The compiler decides, by assignability asked both ways. Neither names nor
 * printed text are compared.
 */
import { ts } from 'ts-morph';
import type { ModelSymbolRelation } from './types.js';
/**
 * The relation of `symbol` (the type the model named) to `read` (the types
 * of the expressions the handler sends, one per surviving branch), or
 * `undefined` where there is nothing to compare: either side is `any` or
 * `unknown` at its root, or is only `null`.
 *
 *  - `same`: the two are one type. A list's own `readonly` does not count.
 *  - `list`: the read is a list of the symbol, `depth` levels deep.
 *  - `member`: the symbol is one member of the read's union, and the rest
 *    is more than `null` or `undefined`. Which member is the success body is
 *    decided by status, elsewhere.
 *  - `nullable`: the read is the symbol or `null`/`undefined`.
 *  - `different`: anything else.
 */
export declare function relateSymbolToRead(checker: ts.TypeChecker, symbol: ts.Type, read: readonly ts.Type[]): ModelSymbolRelation | undefined;
/**
 * Whether a printed type holds `any` or `unknown` anywhere. Such a read has
 * a part the compiler could not see, and assignability against it says
 * nothing about the part it could.
 */
export declare function printedTypeHoldsTopType(text: string): boolean;
