/**
 * Which declaration a symbol is attributed to, and which installed package a
 * declaring file belongs to. Shared by the inferrer's `declaring_package`
 * answer and the per-position rows (carrick#2405), so the two can never name
 * different declarations or packages for one symbol.
 */
import type { ts } from 'ts-morph';
/**
 * The declaration a symbol is attributed to (carrick#2031). One declaration
 * is itself. Where several files merge one symbol (a global interface the
 * compiler's library declares and a typings package augments), their order
 * is the order the checker merged them in, and on TypeScript 6 that changes
 * once the program is rebuilt, so the first one is not an answer. The
 * compiler's default library wins where it declares the symbol; otherwise,
 * and among the library's own files, the declaring file whose path sorts
 * first, then the earliest declaration in it.
 *
 * `programOf` is called only when there are several: asking a ts-morph
 * project for its program rebuilds one that has changed since, and a symbol
 * with one declaration needs none.
 */
export declare function attributedDeclaration<T extends ts.Node>(programOf: () => ts.Program, declarations: readonly T[]): T | undefined;
/**
 * The declaration a member is attributed to when it is read on `holder`: its
 * own ({@link attributedDeclaration}), or, for a member with none (a mapped
 * type's), the declaration of the holder's type: its symbol's, else its alias
 * symbol's (carrick#2448). The per-position rows and a library verdict's
 * `declarations` (carrick#2654) both name a member this way, so a verdict
 * joins the row of the call it judges.
 */
export declare function memberDeclaration(programOf: () => ts.Program, member: ts.Symbol, holder: ts.Type): ts.Declaration | undefined;
/** The declaration of a type's symbol, else of its alias symbol: a mapped type node, or the alias naming it. */
export declare function typeDeclaration(programOf: () => ts.Program, type: ts.Type): ts.Declaration | undefined;
/**
 * The npm package a declaring file belongs to, by the module graph where
 * there is one and by its path otherwise. `undefined` when neither names one.
 *
 * The module graph is asked first, where there is one, because it is the
 * only thing that can answer for a project whose resolution does not go
 * through `node_modules`. Deno resolves an npm dependency's types straight
 * out of its own cache (`$DENO_DIR/npm/<registry-host>/<name>/<version>/…`),
 * a path with no `node_modules` segment anywhere in it (carrick#1260). It
 * runs first rather than second because a mixed checkout has both, and the
 * graph is what actually resolved the module.
 *
 * The path rule is the Node answer: the LAST `node_modules` segment wins,
 * which is what a nested or content-addressed store
 * (`node_modules/.store/pkg@1.0.0/node_modules/pkg`) requires. Scoped names
 * keep both segments; a scope with no name after it names nothing.
 */
export declare function installedPackageOf(filePath: string, packageOf?: (filePath: string) => string | undefined): string | undefined;
