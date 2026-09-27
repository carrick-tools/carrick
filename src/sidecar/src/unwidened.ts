/**
 * The unwidened reading of a producer's response (carrick#1516).
 *
 * A handler that returns `{ scope: row.parentId ? 'specific' : 'all' }` sends
 * one of two strings, but its inferred return type says `scope: string`:
 * TypeScript widens a literal that lands in a mutable position (an object
 * property, an array element, a function's return). The index publishes that
 * widened type, so a consumer declaring `scope: 'all' | 'specific'` is judged
 * against a `string` the handler never sends.
 *
 * The published type stays what the compiler inferred. Beside it, this reads
 * the SAME inference again with every literal on the handler's path marked
 * `as const`, which is the compiler's own way of saying "keep this literal's
 * type". A const-asserted literal is a regular literal type, and only fresh
 * literal types widen, so the re-read inference is the handler's return with
 * nothing widened: `'all' | 'specific'` for the conditional, `true`/`false`
 * for a discriminant, `2 | 1` for a numeric choice. No rule of ours decides
 * what a literal becomes; the compiler infers the return again.
 *
 * The judge uses the reading only to classify a mismatch the published type
 * already raised: when this narrower type fits the consumer, the pair is
 * reported as the producer's type being wider than what it sends, not as a
 * break.
 *
 * Soundness rests on three things:
 *
 * - **A literal that feeds a mutable binding keeps its widening.** `let s =
 *   'a'` can later hold `'b'`, so its type really is `string`; the literals
 *   that initialise a `let`/`var` are never marked. Nor is a parameter's
 *   default: it decides what callers may pass, and a caller the path does not
 *   reach could pass another value.
 * - **No new diagnostic on the handler's path.** A marked literal that is
 *   later changed (an object built with `'a'` and then assigned `'b'`) makes
 *   the compiler report the assignment. Every file holding a function on the
 *   path is type-checked before and after, and any diagnostic the marking adds
 *   drops the reading for every request whose path includes that function.
 * - **The re-read came from the same place.** The inference is re-run from
 *   the request, so its locator runs again; the reading is kept only when it
 *   read the same node it read the first time.
 *
 * The path is the function the inference read (or the function around the
 * payload expression it read), plus every source function it calls whose
 * return type is inferred rather than declared, transitively: a controller
 * that returns `this.service.list()` sends what `list` returns. A callee with
 * a declared return type is not followed; that declaration is the contract.
 */

import { Node, SyntaxKind, ts } from 'ts-morph';
import type { SourceFile } from 'ts-morph';

/** How many calls deep the path follows an un-annotated callee. */
const MAX_FOLLOW_DEPTH = 3;
/** Functions one request's path may hold before following stops. */
const MAX_PATH_FUNCTIONS = 32;

const OPEN = '(';
const CLOSE = ' as const)';

/** One insertion into a file's ORIGINAL text. */
export interface Insertion {
  pos: number;
  text: string;
  /**
   * An opener sits before the literal it wraps, a closer after it. At one
   * position a closer (ending the previous literal) comes before an opener.
   */
  closes: boolean;
}

/** A function on a request's path, by its original span. */
export interface PathFunction {
  file: SourceFile;
  start: number;
  end: number;
}

/** One diagnostic a file's type-check reported. */
export interface FileDiagnostic {
  /** Start position in the file it was reported on. */
  start: number;
  code: number;
  message: string;
}

/**
 * The functions on the path of a read node: the function it is (or sits in),
 * then every un-annotated source function called from those, breadth first.
 * A read node outside any function is its own path.
 */
export function pathOf(read: Node): Node[] {
  const root = isFunctionWithBody(read) ? read : read.getFirstAncestor(isFunctionWithBody) ?? read;
  const seen = new Set<Node>([root]);
  const out: Node[] = [root];
  let frontier: Node[] = [root];
  for (let depth = 0; depth < MAX_FOLLOW_DEPTH && frontier.length > 0; depth++) {
    const next: Node[] = [];
    for (const fn of frontier) {
      for (const callee of calleesOf(fn)) {
        if (seen.has(callee) || out.length >= MAX_PATH_FUNCTIONS) continue;
        seen.add(callee);
        out.push(callee);
        next.push(callee);
      }
    }
    frontier = next;
  }
  return out;
}

/**
 * The source functions `fn` calls whose return type the compiler inferred. A
 * declared return type is the callee's contract: nothing in it was widened,
 * and marking its literals could only cost the reading a diagnostic.
 */
function calleesOf(fn: Node): Node[] {
  const out: Node[] = [];
  for (const call of fn.getDescendantsOfKind(SyntaxKind.CallExpression)) {
    const node = call.getProject().getTypeChecker().getResolvedSignature(call)?.getDeclaration();
    if (!node || !isFunctionWithBody(node) || hasDeclaredReturn(node)) continue;
    const file = node.getSourceFile();
    if (file.isDeclarationFile() || file.isFromExternalLibrary() || file.isInNodeModules()) continue;
    out.push(node);
  }
  return out;
}

function isFunctionWithBody(node: Node): boolean {
  return (
    (Node.isFunctionDeclaration(node) ||
      Node.isMethodDeclaration(node) ||
      Node.isArrowFunction(node) ||
      Node.isFunctionExpression(node)) &&
    node.getBody() !== undefined
  );
}

function hasDeclaredReturn(node: Node): boolean {
  return (
    (Node.isFunctionDeclaration(node) ||
      Node.isMethodDeclaration(node) ||
      Node.isArrowFunction(node) ||
      Node.isFunctionExpression(node)) &&
    node.getReturnTypeNode() !== undefined
  );
}

/**
 * The insertions that mark every literal inside `fn` whose widening the
 * reading drops. Positions are original-file positions.
 */
export function literalInsertions(fn: Node): Insertion[] {
  const out: Insertion[] = [];
  const visit = (node: Node): void => {
    // A type states no value, and a parameter's default decides what its
    // callers may pass, not what the function returns.
    if (Node.isTypeNode(node) || Node.isParameterDeclaration(node)) return;
    if (isMarkableLiteral(node)) {
      out.push({ pos: node.getStart(), text: OPEN, closes: false });
      out.push({ pos: node.getEnd(), text: CLOSE, closes: true });
      return;
    }
    node.forEachChild(visit);
  };
  visit(fn);
  return out;
}

/** Whether `node` is a literal value the reading marks `as const`. */
function isMarkableLiteral(node: Node): boolean {
  const kind = node.getKind();
  const literal =
    kind === SyntaxKind.StringLiteral ||
    kind === SyntaxKind.NumericLiteral ||
    kind === SyntaxKind.BigIntLiteral ||
    kind === SyntaxKind.NoSubstitutionTemplateLiteral ||
    kind === SyntaxKind.TrueKeyword ||
    kind === SyntaxKind.FalseKeyword ||
    // `-1` is marked whole: `-(1 as const)` is a `number`, and the visit
    // never reaches the `1` inside it.
    (Node.isPrefixUnaryExpression(node) &&
      node.getOperatorToken() === SyntaxKind.MinusToken &&
      (Node.isNumericLiteral(node.getOperand()) || Node.isBigIntLiteral(node.getOperand())));
  if (!literal) return false;
  const parent = node.getParent();
  if (!parent) return false;
  // Syntax, not a value: a quoted property name, a tagged template's text.
  if ((parent as Node & { getNameNode?(): Node }).getNameNode?.() === node) return false;
  if (Node.isTaggedTemplateExpression(parent)) return false;
  return !initialisesMutableBinding(node);
}

/**
 * Whether the literal becomes the value of a `let`/`var` binding, which can
 * later hold another one, so its type really is the widened one. Reached
 * through the expressions that pass a value through unchanged.
 */
function initialisesMutableBinding(literal: Node): boolean {
  let node: Node = literal;
  for (;;) {
    const parent = node.getParent();
    if (!parent) return false;
    if (
      Node.isParenthesizedExpression(parent) ||
      (Node.isConditionalExpression(parent) && parent.getCondition() !== node) ||
      (Node.isBinaryExpression(parent) && passesOperandThrough(parent.getOperatorToken().getKind()))
    ) {
      node = parent;
      continue;
    }
    if (!Node.isVariableDeclaration(parent) || parent.getInitializer() !== node) return false;
    const list = parent.getParent();
    return Node.isVariableDeclarationList(list) && (list.getFlags() & ts.NodeFlags.Const) === 0;
  }
}

/** Binary operators whose value is one of their operands. */
function passesOperandThrough(kind: SyntaxKind): boolean {
  return (
    kind === SyntaxKind.BarBarToken ||
    kind === SyntaxKind.QuestionQuestionToken ||
    kind === SyntaxKind.AmpersandAmpersandToken ||
    kind === SyntaxKind.CommaToken
  );
}

/** Deduplicated, in text order (a closer before an opener at one position). */
export function normalise(insertions: Insertion[]): Insertion[] {
  const seen = new Set<string>();
  const out: Insertion[] = [];
  for (const insertion of insertions) {
    const k = `${insertion.pos}:${insertion.closes}`;
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(insertion);
  }
  return out.sort((a, b) => a.pos - b.pos || Number(b.closes) - Number(a.closes));
}

export function applyInsertions(text: string, insertions: Insertion[]): string {
  let out = '';
  let cursor = 0;
  for (const insertion of insertions) {
    out += text.slice(cursor, insertion.pos) + insertion.text;
    cursor = insertion.pos;
  }
  return out + text.slice(cursor);
}

/**
 * Where an original position lands in the rewritten text. A START keeps an
 * opener at its own position outside (so a span starting on a marked literal
 * covers its parentheses); an END takes every insertion at its position in.
 */
export function mapForward(pos: number, insertions: Insertion[], side: 'start' | 'end'): number {
  let shift = 0;
  for (const insertion of insertions) {
    if (insertion.pos < pos || (insertion.pos === pos && (side === 'end' || insertion.closes))) {
      shift += insertion.text.length;
    }
  }
  return pos + shift;
}

/** The original position a rewritten one came from; inside an insertion, its position. */
export function mapBack(pos: number, insertions: Insertion[]): number {
  let shift = 0;
  for (const insertion of insertions) {
    const at = insertion.pos + shift;
    if (pos < at) break;
    if (pos < at + insertion.text.length) return insertion.pos;
    shift += insertion.text.length;
  }
  return pos - shift;
}

/** Syntactic and semantic diagnostics reported in this file. */
export function fileDiagnostics(sourceFile: SourceFile): FileDiagnostic[] {
  const program = sourceFile.getProject().getProgram().compilerObject;
  const node = sourceFile.compilerNode;
  return [...program.getSyntacticDiagnostics(node), ...program.getSemanticDiagnostics(node)]
    .filter((d) => d.file === node && d.start !== undefined)
    .map((d) => ({
      start: d.start!,
      code: d.code,
      message: ts.flattenDiagnosticMessageText(d.messageText, ' '),
    }));
}

/**
 * Original positions of the diagnostics `after` holds that `before` does not,
 * matched by original position and code. A diagnostic whose message changed
 * because a type narrowed is the same diagnostic.
 */
export function addedDiagnostics(
  before: FileDiagnostic[],
  after: FileDiagnostic[],
  insertions: Insertion[]
): number[] {
  const remaining = new Map<string, number>();
  for (const d of before) {
    const k = `${d.start}:${d.code}`;
    remaining.set(k, (remaining.get(k) ?? 0) + 1);
  }
  const added: number[] = [];
  for (const d of after) {
    const start = mapBack(d.start, insertions);
    const k = `${start}:${d.code}`;
    const left = remaining.get(k) ?? 0;
    if (left > 0) {
      remaining.set(k, left - 1);
      continue;
    }
    added.push(start);
  }
  return added;
}
