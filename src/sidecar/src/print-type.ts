/**
 * The compiler's own print of a type, with a module the print names written
 * as the file that declares it (carrick#2019).
 *
 * When a type comes from a module the enclosing file does not import, the
 * compiler writes it as `import("<module>").Name`. TypeScript 5 wrote the
 * declaring file's path there: the node builder had no module-specifier host
 * unless a declaration emit gave it one. TypeScript 6 gives every print a
 * host, so the same type printed at an enclosing node reads
 * `import("../model").Name` or `import("<package>").Name`: a specifier that
 * depends on which file it was printed from, so one type printed from two
 * files gives two texts. Everything downstream reads the declaring file's
 * path: the scanner's path scrub writes it repo-relative or as
 * `<package>@<version>`, and a capture pastes an inferred text into its
 * surface entry, where a specifier relative to another file names nothing.
 *
 * These functions run the compiler's own code with no host: a tracker whose
 * `moduleResolverHost` is `false` (falsy, so the builder writes the declaring
 * file; not nullish, so the builder does not supply its own). Nothing in the
 * text is rewritten afterwards. `typeToStringAsDeclared` is the checker's
 * `typeToString`, step for step, with that one tracker.
 *
 * Only prints with an enclosing node differ: with none, the compiler has no
 * file to write a specifier relative to and writes the declaring file
 * anyway. A print that must name modules the way a declaration file would
 * (the capture's surface emit) passes a host of its own and does not use
 * this.
 */
import { ts } from 'ts-morph';

/**
 * The compiler's internal `SymbolTracker`: callbacks the node builder reports
 * to, and the module-specifier host it writes specifiers with.
 */
export interface SymbolTracker {
  moduleResolverHost?: unknown;
}

/** A symbol tracker that leaves the node builder without a module-specifier host. */
const NO_SPECIFIER_HOST: SymbolTracker = { moduleResolverHost: false };

/** The checker's own default for `typeToString`. */
const DEFAULT_TYPE_FORMAT =
  ts.TypeFormatFlags.AllowUniqueESSymbolType | ts.TypeFormatFlags.UseAliasDefinedOutsideCurrentScope;

/** `typeToString`'s caps: a print this long is cut, `...` included. */
const DEFAULT_MAXIMUM_LENGTH = 160;
const NO_TRUNCATION_MAXIMUM_LENGTH = 1_000_000;

interface NodeBuilderChecker {
  typeToTypeNode(
    type: ts.Type,
    enclosingDeclaration: ts.Node | undefined,
    flags: ts.NodeBuilderFlags | undefined,
    internalFlags?: number,
    tracker?: SymbolTracker
  ): ts.TypeNode | undefined;
}

interface Writer {
  getText(): string;
}

interface InternalTs {
  createTextWriter(newLine: string): Writer;
}

interface WritingPrinter extends ts.Printer {
  writeNode(hint: ts.EmitHint, node: ts.Node, sourceFile: ts.SourceFile | undefined, writer: Writer): void;
}

/** `typeToString`'s printers: comments removed, except for the unresolved type. */
const PRINTER = ts.createPrinter({ removeComments: true }) as WritingPrinter;
const PRINTER_WITH_COMMENTS = ts.createPrinter({}) as WritingPrinter;

/**
 * The checker's `typeToTypeNode`, writing each module a node names as the
 * file that declares it. A caller's own tracker keeps its callbacks.
 */
export function typeToTypeNodeAsDeclared(
  checker: ts.TypeChecker,
  type: ts.Type,
  enclosing: ts.Node | undefined,
  flags: ts.NodeBuilderFlags,
  tracker?: SymbolTracker
): ts.TypeNode | undefined {
  const withoutHost = tracker
    ? (Object.assign(Object.create(tracker), { moduleResolverHost: false }) as SymbolTracker)
    : NO_SPECIFIER_HOST;
  return (checker as unknown as NodeBuilderChecker).typeToTypeNode(type, enclosing, flags, undefined, withoutHost);
}

/**
 * The checker's `typeToString(type, enclosing, flags)`, writing each module
 * the print names as the file that declares it. `noErrorTruncation` is the
 * program's option of that name, which `typeToString` reads when `flags`
 * leave truncation on.
 */
export function typeToStringAsDeclared(
  checker: ts.TypeChecker,
  type: ts.Type,
  enclosing: ts.Node | undefined,
  flags: ts.TypeFormatFlags = DEFAULT_TYPE_FORMAT,
  noErrorTruncation = false
): string {
  const noTruncation = noErrorTruncation || (flags & ts.TypeFormatFlags.NoTruncation) !== 0;
  const node = typeToTypeNodeAsDeclared(
    checker,
    type,
    enclosing,
    (flags & ts.TypeFormatFlags.NodeBuilderFlagsMask) |
      ts.NodeBuilderFlags.IgnoreErrors |
      (noTruncation ? ts.NodeBuilderFlags.NoTruncation : 0)
  );
  // `typeToString` asserts a node; with IgnoreErrors the builder always gives one.
  if (node === undefined) return checker.typeToString(type, enclosing, flags);
  const unresolved = (type as { intrinsicName?: string }).intrinsicName === 'unresolved';
  const writer = (ts as unknown as InternalTs).createTextWriter('');
  (unresolved ? PRINTER_WITH_COMMENTS : PRINTER).writeNode(
    ts.EmitHint.Unspecified,
    node,
    enclosing?.getSourceFile(),
    writer
  );
  const result = writer.getText();
  const maximum = (noTruncation ? NO_TRUNCATION_MAXIMUM_LENGTH : DEFAULT_MAXIMUM_LENGTH) * 2;
  return result.length >= maximum ? result.substring(0, maximum - 3) + '...' : result;
}
