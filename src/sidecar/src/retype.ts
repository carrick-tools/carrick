/**
 * The retype check (carrick#1491): judge an untyped consumer call by what its
 * own source does with the producer's response.
 *
 * A consumer that writes `await api.post('/orders')` with no type argument
 * gets `any` back, so the pair has nothing to compare and every field it reads
 * goes unchecked. The source still says how the response is used: the members
 * it reads, what it destructures, the typed places it flows into. So the call
 * is rewritten, in memory, to state the producer's response type
 * (`api.post<ProducerResponse>('/orders')`), the consumer file is type-checked
 * before and after, and every diagnostic the rewrite adds is a place where the
 * consumer uses something the producer does not return. No rule of ours
 * decides anything: the compiler reads the consumer's own code.
 *
 * A call that takes no type argument but returns a typed transport response
 * (`const res = await fetch(u)`) is judged at its body read instead: the
 * `res.json()` on that call's result is the payload, so that read is cast to
 * the producer's type (carrick#1493).
 *
 * Runs in the consumer's REAL program (the init'd project), because that is
 * the only place its file can type-check: the check workspace holds
 * declaration stubs, not consumer sources, and none of the consumer's imports.
 *
 * Every edit is undone before the next item and before returning, so the
 * project other requests read is the project the scan loaded.
 */

import { Node, SyntaxKind, ts } from 'ts-morph';
import type { CallExpression, Identifier, Project, PropertyAccessExpression, SourceFile, Type } from 'ts-morph';
import type { TypeInferrer } from './type-inferrer.js';
import type {
  InferRequestItem,
  RetypeDiagnostic,
  RetypeItem,
  RetypeOutcome,
} from './types.js';

/** Names appended to a file that is not ours carry this prefix. */
const PREFIX = '__carrick_';
const WIRE = `${PREFIX}Wire`;

/** One text edit, in ORIGINAL-file positions. */
interface Edit {
  start: number;
  end: number;
  text: string;
  /** Where the stated producer type begins inside `text`, when it is there. */
  statedAt?: number;
}

/**
 * The check phase's structural any/unknown walk (`findDisqualifyingTopTypes`
 * in the capture bundle): member paths below `root` that hold a top type.
 */
export type TopTypeWalk = (
  root: ts.Type,
  program: ts.Program,
  checker: ts.TypeChecker,
  location: ts.Node
) => Array<{ kind: string; path: string }>;

interface Diag {
  /** Start position in the file the diagnostic was reported on. */
  start: number;
  code: number;
  message: string;
}

/** Where a position in the rewritten text came from. */
type Origin = { kind: 'original'; pos: number } | { kind: 'inserted' };

interface Rewrite {
  edits: Edit[];
  /** The producer's type as the edits state it. */
  stated: string;
  /** The wire transform is stated, so its declarations are appended. */
  wire: boolean;
  /**
   * Start/end of the rewritten call in the NEW text, for re-locating it, when
   * a type argument was stated there. A body-read cast states the type
   * directly, so there is nothing to follow into a result.
   */
  typeArgumentCall?: { start: number; end: number };
}

export class Retyper {
  /**
   * `jsonWire` is the check phase's own JSON wire transform
   * (`jsonWireDeclarations` in the capture bundle) and `topTypes` its
   * any/unknown walk, handed in by the entry point so both judges read the
   * wire and a top type the same way without this file crossing the bundle
   * seam.
   */
  constructor(
    private readonly project: Project,
    private readonly inferrer: TypeInferrer,
    private readonly jsonWire: (prefix: string) => string[],
    private readonly topTypes: TopTypeWalk
  ) {}

  /**
   * Judge every item, spending at most `budgetMs`. Each item rebuilds the
   * program at least twice, so a consumer with many calls could otherwise
   * outrun the caller's read deadline and lose every answer; the items the
   * budget does not reach abstain and say so.
   */
  run(items: RetypeItem[], budgetMs: number): RetypeOutcome[] {
    const deadline = performance.now() + budgetMs;
    // What each file said before any rewrite. Every rewrite is undone, so it
    // is the same for every item in the file.
    const before = new Map<SourceFile, Diag[]>();
    return items.map((item) => {
      if (performance.now() >= deadline) {
        return abstain(item, `the retype check ran out of its ${budgetMs}ms budget`);
      }
      try {
        return this.runOne(item, before);
      } catch (err) {
        return abstain(
          item,
          `the retype check failed: ${err instanceof Error ? err.message : String(err)}`
        );
      }
    });
  }

  private runOne(item: RetypeItem, before: Map<SourceFile, Diag[]>): RetypeOutcome {
    const producer = oneLine(item.producer_type);
    if (!producer) return abstain(item, "the producer's response type is empty");
    if (producer.includes('//') || producer.includes('/*')) {
      // Collapsing the text onto one line would turn a line comment into a
      // comment over the rest of the call.
      return abstain(item, "the producer's response type carries a comment");
    }

    const located = this.inferrer.locateCall(asLocator(item));
    if (!located) return abstain(item, 'the consumer call could not be located in its file');
    const { sourceFile, call } = located;

    if (resultIsDiscarded(call)) {
      // Nothing reads the response, so nothing can disagree with it: an
      // "agrees" here would be a verdict about no comparison at all.
      return abstain(item, 'the consumer never reads the response');
    }
    const escape = resultEscapes(call);
    if (escape) {
      // Only this file's diagnostics are compared, so reads made where the
      // value escapes to are invisible and an "agrees" would claim them.
      return abstain(item, escape);
    }

    const plan = this.plan(call, producer, item.wire);
    if (typeof plan === 'string') return abstain(item, plan);

    const original = sourceFile.getFullText();
    let pre = before.get(sourceFile);
    if (!pre) {
      pre = fileDiagnostics(sourceFile);
      before.set(sourceFile, pre);
    }
    const decisive = this.check(sourceFile, original, plan(true), pre);
    if (decisive.kind === 'abstain') return abstain(item, decisive.reason);
    if (decisive.added.length === 0) {
      return {
        item_id: item.item_id,
        outcome: 'agrees',
        diagnostics: [],
      };
    }

    // The wire form decided it. The DECLARED form prints the producer's own
    // type in each message instead of the transform's name, so its text is
    // used wherever it reports the same place; the decision is not re-made.
    let messages = new Map<string, string>();
    if (item.wire) {
      const plain = this.check(sourceFile, original, plan(false), pre);
      if (plain.kind === 'checked') {
        messages = new Map(plain.added.map((d) => [key(d), d.message]));
      }
    }
    const lineOf = lineIndex(original);
    return {
      item_id: item.item_id,
      outcome: 'mismatch',
      diagnostics: decisive.added.map(
        (d): RetypeDiagnostic => ({
          line: lineOf(d.start),
          code: d.code,
          message: messages.get(key(d)) ?? d.message,
        })
      ),
    };
  }

  /**
   * How to state the producer's type at this call, or why it cannot be done.
   * Returns a builder so the wire and the declared form share one decision.
   */
  private plan(
    call: CallExpression,
    producer: string,
    wire: boolean
  ): ((useWire: boolean) => Rewrite) | string {
    const stated = (useWire: boolean) =>
      useWire && wire ? `${WIRE}<(${producer})>` : `(${producer})`;
    const callStart = call.getStart();
    const callEnd = call.getEnd();

    const typeArgs = call.getTypeArguments();
    if (typeArgs.length > 0) {
      // The source states a type argument already. The producer's type
      // replaces it: what is judged is how the source USES the response, not
      // what it claimed the response was.
      // Positions are read now: the rewrite forgets every node of the file.
      const argStart = typeArgs[0].getStart();
      const argEnd = typeArgs[0].getEnd();
      return (useWire) => {
        const text = stated(useWire);
        const delta = text.length - (argEnd - argStart);
        return {
          edits: [{ start: argStart, end: argEnd, text, statedAt: 0 }],
          stated: text,
          wire: useWire && wire,
          typeArgumentCall: { start: callStart, end: callEnd + delta },
        };
      };
    }

    if (declaresTypeParameters(call)) {
      const open = call.getFirstChildByKind(SyntaxKind.OpenParenToken);
      if (!open) return 'the consumer call has no argument list to retype';
      const at = open.getStart();
      return (useWire) => {
        const text = `<${stated(useWire)}>`;
        return {
          edits: [{ start: at, end: at, text, statedAt: 1 }],
          stated: stated(useWire),
          wire: useWire && wire,
          typeArgumentCall: { start: callStart, end: callEnd + text.length },
        };
      };
    }

    // No type parameter to state. A cast of the call's result is not
    // offered: an untyped helper declared `Promise<any>` may return a whole
    // client envelope, and casting that to the payload would report every
    // `.data` read as a break (ruling on PR #1492).
    if (!resolvedDeclaration(call)) {
      return 'the consumer call does not resolve in its program (is the client installed?)';
    }
    // A typed transport response (`fetch`) whose body the source reads with
    // `.json()`: the payload is that read, so the cast goes there
    // (carrick#1493). The call's own result is not the payload and is not
    // touched.
    const reads = bodyReadsOf(call);
    if (typeof reads === 'string') return reads;
    if (reads.length > 0) {
      const edits = reads.map(bodyReadEdit);
      return (useWire) => {
        const text = stated(useWire);
        return {
          edits: edits.flatMap((edit) => edit(text)),
          stated: text,
          wire: useWire && wire,
        };
      };
    }
    return (
      `the consumer call takes no type argument and returns ` +
      `'${call.getType().getText(call)}', so it cannot be retyped`
    );
  }

  /**
   * Apply one rewrite, type-check, restore. Returns the diagnostics the
   * rewrite ADDED, in original-file positions.
   */
  private check(
    sourceFile: SourceFile,
    original: string,
    rewrite: Rewrite,
    pre: Diag[]
  ):
    | { kind: 'checked'; added: Diag[] }
    | { kind: 'abstain'; reason: string } {
    // Appended only when the rewrite names them: an alias nothing references
    // is a TS6196 under `noUnusedLocals`, which would read as the producer's
    // type failing to resolve.
    const appendix = rewrite.wire
      ? '\n' +
        [
          ...this.jsonWire(PREFIX),
          `type ${WIRE}<P> = ${PREFIX}JsonWireSame<${PREFIX}JsonWire<P>, P> extends true ? P : ${PREFIX}JsonWire<P>;`,
        ].join('\n') +
        '\n'
      : '';
    const rewritten = applyEdits(original, rewrite.edits) + appendix;
    const bodyEnd = rewritten.length - appendix.length;
    const originOf = (pos: number): Origin =>
      pos >= bodyEnd ? { kind: 'inserted' } : mapBack(pos, rewrite.edits);

    sourceFile.replaceWithText(rewritten);
    try {
      if (rewrite.typeArgumentCall) {
        const reached = this.typeArgumentReachesResult(sourceFile, rewrite.typeArgumentCall);
        if (reached !== true) {
          return { kind: 'abstain', reason: reached };
        }
      }

      const post = fileDiagnostics(sourceFile);
      const inserted = post.filter((d) => originOf(d.start).kind === 'inserted');
      if (inserted.length > 0) {
        return {
          kind: 'abstain',
          reason:
            "the producer's response type does not resolve in the consumer's program: " +
            `TS${inserted[0].code}: ${inserted[0].message}`,
        };
      }

      const remaining = new Map<string, number>();
      for (const d of pre) remaining.set(key(d), (remaining.get(key(d)) ?? 0) + 1);
      const added: Diag[] = [];
      for (const d of post) {
        const origin = originOf(d.start) as { kind: 'original'; pos: number };
        const mapped = { ...d, start: origin.pos };
        const k = key(mapped);
        const left = remaining.get(k) ?? 0;
        if (left > 0) {
          remaining.set(k, left - 1);
          continue;
        }
        added.push(mapped);
      }
      added.sort((a, b) => a.start - b.start || a.code - b.code);

      const unresolved = this.unresolvedMembers(sourceFile, rewrite, added.length > 0);
      if (unresolved) return { kind: 'abstain', reason: unresolved };
      return { kind: 'checked', added };
    } finally {
      sourceFile.replaceWithText(original);
    }
  }

  /**
   * Why the stated type, as the consumer's program reads it, cannot be
   * compared, or `undefined` when it can (carrick#1514). Read after the
   * diagnostics diff, because the two top types fail in opposite directions.
   *
   * The producer's text is sent only when it holds no `any`/`unknown`, but
   * the consumer's program is where it is read: under its compiler options,
   * through the wire transform (a lib type whose `toJSON()` returns `any`),
   * with its own declarations.
   *
   * - A member that reads as `unknown` makes reads fail that the producer's
   *   type would pass, so any diagnostic may be ours. It is never compared.
   * - A member that reads as `any` can hide a failure but never make one, so
   *   the diagnostics the diff found stand; only an empty diff is not
   *   compared, since it may be the `any` agreeing.
   *
   * Either way the reason names the member, as the check phase does for a
   * published type.
   *
   * The walk's budget sentinel is dropped. The scanner's text screen
   * (`contains_disqualifying_top_type`, which has no budget) found no
   * `any`/`unknown` in the producer's text before sending it, so a walk that
   * runs out of budget can only miss one the consumer's program made deeper
   * than the budget allows; the compiler's diagnostics stand.
   */
  private unresolvedMembers(
    sourceFile: SourceFile,
    rewrite: Rewrite,
    diagnosed: boolean
  ): string | undefined {
    let shift = 0;
    let at: number | undefined;
    for (const edit of [...rewrite.edits].sort((a, b) => a.start - b.start)) {
      if (at === undefined && edit.statedAt !== undefined) {
        at = edit.start + shift + edit.statedAt;
      }
      shift += edit.text.length - (edit.end - edit.start);
    }
    const node =
      at === undefined
        ? undefined
        : sourceFile.getDescendantAtStartWithWidth(at, rewrite.stated.length);
    // Every rewrite states the type at a recorded offset, so this is only
    // reached through a wrong offset; it abstains rather than skip the walk.
    if (!node) return 'the stated producer type could not be found again after the rewrite';

    const type = node.getType().compilerType;
    let found: Array<{ kind: string; path: string }>;
    if (type.flags & ts.TypeFlags.Unknown) found = [{ kind: 'unknown', path: '' }];
    else if (type.flags & ts.TypeFlags.Any) found = [{ kind: 'any', path: '' }];
    else {
      const program = this.project.getProgram().compilerObject;
      found = this.topTypes(type, program, program.getTypeChecker(), node.compilerNode);
    }
    const unknowns = found.filter((finding) => finding.kind === 'unknown');
    const anys = found.filter((finding) => finding.kind === 'any');
    const deciding = unknowns.length > 0 ? unknowns : diagnosed ? [] : anys;
    if (deciding.length === 0) return undefined;

    const members = deciding
      .map(({ kind, path }) => (path === '' ? `'${kind}'` : `'${kind}' at '${path}'`))
      .join(', ');
    return `the producer's response reads as ${members} in the consumer's program, so it was not compared`;
  }

  /**
   * The stated type argument must be what the call RETURNS (or a type
   * argument or member of it): the rewrite is only a statement about the
   * response when the parameter it fills carries the response. A parameter
   * that types something else — a request body, a config — would judge the
   * consumer against the wrong thing, so the item abstains.
   */
  private typeArgumentReachesResult(
    sourceFile: SourceFile,
    at: { start: number; end: number }
  ): true | string {
    const call = sourceFile
      .getDescendantsOfKind(SyntaxKind.CallExpression)
      .find((c) => c.getStart() === at.start && c.getEnd() === at.end);
    if (!call) return 'the retyped call could not be found again after the rewrite';
    const stated = call.getTypeArguments()[0]?.getType();
    if (!stated) return 'the retyped call carries no type argument';
    const checker = this.project.getTypeChecker().compilerObject;
    const returned = call.getType().compilerType;
    const result = checker.getAwaitedType(returned) ?? returned;
    const target = stated.compilerType;
    if (result === target) return true;
    const reference = result as ts.TypeReference;
    const args = [
      ...(result.aliasTypeArguments ?? []),
      ...(result.flags & ts.TypeFlags.Object &&
      (result as ts.ObjectType).objectFlags & ts.ObjectFlags.Reference
        ? checker.getTypeArguments(reference)
        : []),
    ];
    if (args.includes(target)) return true;
    for (const property of checker.getPropertiesOfType(result)) {
      if (checker.getTypeOfSymbolAtLocation(property, call.compilerNode) === target) return true;
    }
    return (
      "the call's type argument does not reach its result " +
      `('${checker.typeToString(result)}'), so it does not state the response`
    );
  }
}

function abstain(item: RetypeItem, reason: string): RetypeOutcome {
  return { item_id: item.item_id, outcome: 'abstain', diagnostics: [], reason };
}

function asLocator(item: RetypeItem): InferRequestItem {
  return {
    file_path: item.file_path,
    line_number: item.line_number,
    span_start: item.span_start,
    span_end: item.span_end,
    expression_text: item.expression_text,
    expression_line: item.expression_line,
    infer_kind: 'call_result',
  };
}

/** Whitespace collapsed, so the rewrite never moves a line. */
function oneLine(text: string): string {
  return text.replace(/\s+/g, ' ').trim().replace(/;$/, '').trim();
}

/** `await call;` / `call;` / `void call;` — the value goes nowhere. */
function resultIsDiscarded(call: CallExpression): boolean {
  let node: Node = call;
  let parent = node.getParent();
  while (
    parent &&
    (Node.isAwaitExpression(parent) ||
      Node.isParenthesizedExpression(parent) ||
      Node.isVoidExpression(parent))
  ) {
    if (Node.isVoidExpression(parent)) return true;
    node = parent;
    parent = node.getParent();
  }
  return !!parent && Node.isExpressionStatement(parent);
}

function resolvedDeclaration(call: CallExpression): ts.SignatureDeclaration | undefined {
  const signature = call
    .getProject()
    .getTypeChecker()
    .compilerObject.getResolvedSignature(call.compilerNode);
  return signature?.getDeclaration() as ts.SignatureDeclaration | undefined;
}

/**
 * Why the call's result leaves what this file's type-check can see, or
 * `undefined` when every use of it stays in view.
 *
 * The retype diffs one file's diagnostics. A value returned from a function
 * whose return type is inferred carries the producer's type to the callers,
 * wherever they are, and one bound to an exported name carries it out of the
 * file. A function that DECLARES its return type is the opposite: the return
 * is checked against the declaration right here, which is how a typed wrapper
 * is judged. A callback handed to a call returns into that call, which is
 * also in view.
 */
function resultEscapes(call: CallExpression, throughCasts = false): string | undefined {
  let top: Node = call;
  while (
    Node.isAwaitExpression(top.getParentOrThrow()) ||
    Node.isParenthesizedExpression(top.getParentOrThrow()) ||
    (throughCasts && Node.isAsExpression(top.getParentOrThrow()))
  ) {
    top = top.getParentOrThrow();
  }
  if (returnsUndeclared(top)) {
    return 'the response is returned from a function with no declared return type, so its readers are elsewhere';
  }
  const parent = top.getParent();
  if (!parent || !Node.isVariableDeclaration(parent) || parent.getInitializer() !== top) {
    return undefined;
  }
  if (parent.getVariableStatement()?.isExported()) {
    return 'the response is bound to an exported name, so its readers are elsewhere';
  }
  const names = Node.isIdentifier(parent.getNameNode())
    ? [parent.getNameNode()]
    : parent.getNameNode().getDescendantsOfKind(SyntaxKind.Identifier);
  for (const name of names) {
    if (!Node.isIdentifier(name)) continue;
    for (const ref of name.findReferencesAsNodes()) {
      if (returnsUndeclared(ref)) {
        return 'the response is returned from a function with no declared return type, so its readers are elsewhere';
      }
    }
  }
  return undefined;
}

/** `node` is (part of) what a function with an inferred return type returns. */
function returnsUndeclared(node: Node): boolean {
  for (let at: Node | undefined = node; at; at = at.getParent()) {
    const parent = at.getParent();
    if (!parent) return false;
    const returned =
      Node.isReturnStatement(parent) ||
      (Node.isArrowFunction(parent) && parent.getBody() === at);
    if (returned) {
      const fn = Node.isReturnStatement(parent)
        ? parent.getFirstAncestor(
            (n) =>
              Node.isFunctionDeclaration(n) ||
              Node.isFunctionExpression(n) ||
              Node.isArrowFunction(n) ||
              Node.isMethodDeclaration(n)
          )
        : parent;
      if (!fn || !('getReturnTypeNode' in fn)) return false;
      const declared = (fn as unknown as { getReturnTypeNode(): Node | undefined }).getReturnTypeNode();
      if (declared) return false;
      // A callback returns into the call it is handed to, which is in view.
      return !Node.isCallExpression(fn.getParent());
    }
    // A statement inside a block is not what the block's function returns.
    if (Node.isBlock(parent)) return false;
  }
  return false;
}

/**
 * The `.json()` body reads that carry this call's payload, or why the ones
 * the source makes cannot be judged (carrick#1493). An empty list means the
 * source reads no JSON body off the result.
 *
 * The located call may be the body read itself (`res.json()`), or the call
 * whose result the source reads it from: `(await fetch(u)).json()`, or
 * `const res = await fetch(u)` followed by `res.json()` on that binding.
 *
 * A read counts only when it is on THIS call's result: the binding is a
 * plain name that is never reassigned, and every other use of it is a member
 * read (`res.ok`, `res.status`). A response handed whole to anything else
 * may have its body read out of view, so an agreement here would claim reads
 * the diff cannot see.
 */
function bodyReadsOf(call: CallExpression): CallExpression[] | string {
  if (isBodyRead(call)) return checkedBodyReads([call]);

  let top: Node = call;
  while (
    Node.isAwaitExpression(top.getParentOrThrow()) ||
    Node.isParenthesizedExpression(top.getParentOrThrow())
  ) {
    top = top.getParentOrThrow();
  }
  const chained = bodyReadOn(top);
  if (chained) return checkedBodyReads([chained]);

  const declaration = top.getParent();
  if (
    !declaration ||
    !Node.isVariableDeclaration(declaration) ||
    declaration.getInitializer() !== top
  ) {
    return [];
  }
  const name = declaration.getNameNode();
  if (!Node.isIdentifier(name)) return [];

  const reads: CallExpression[] = [];
  let handedOn = false;
  for (const ref of name.findReferencesAsNodes()) {
    const read = bodyReadOn(ref);
    if (read) {
      reads.push(read);
      continue;
    }
    const parent = ref.getParent();
    if (
      parent &&
      Node.isBinaryExpression(parent) &&
      parent.getLeft() === ref &&
      isAssignmentOperator(parent.getOperatorToken().getKind())
    ) {
      return 'the response binding is reassigned, so its body may not be this call\'s payload';
    }
    if (!parent || !Node.isPropertyAccessExpression(parent) || parent.getExpression() !== ref) {
      handedOn = true;
    }
  }
  if (reads.length === 0) return [];
  if (handedOn) {
    return 'the response object is handed on, so its body may be read elsewhere';
  }
  return checkedBodyReads(reads);
}

/**
 * Why these body reads cannot be retyped, or the reads to retype.
 *
 * Only the SUCCESS path's read carries the producer's response (ruling on
 * PR #1505: "cast the body read, never the Response, success path only"). A
 * read under a failed-status test (`if (!res.ok) { const e = await
 * res.json(); ... }`), or one whose result is used only there, parses an
 * error body the producer's response type does not describe, so it is left
 * alone. A read whose result is used on BOTH paths cannot be retyped without
 * judging the error branch against the success type, so the item abstains.
 */
function checkedBodyReads(all: CallExpression[]): CallExpression[] | string {
  const checker = all[0].getProject().getTypeChecker().compilerObject;
  const isResponse = responseTest(all[0]);
  const reads: CallExpression[] = [];
  let errorReads = 0;
  for (const read of all) {
    const side = sideOf(read, isResponse);
    if (side === 'failure') {
      errorReads++;
      continue;
    }
    if (side === 'unclear') {
      return 'the body read sits under a status test whose failing side is unclear';
    }
    const uses = resultNames(read).flatMap((name) => name.findReferencesAsNodes());
    const sides = uses.map((use) => sideOf(use, isResponse));
    if (uses.length > 0 && sides.every((s) => s === 'failure')) {
      errorReads++;
      continue;
    }
    if (sides.some((s) => s === 'failure' || s === 'unclear')) {
      return 'the body read serves both the success and the error path, so it is not retyped';
    }
    reads.push(read);
  }
  if (reads.length === 0) {
    return errorReads > 0
      ? 'the consumer reads the response body only on its error path'
      : 'the consumer never reads the response body';
  }

  for (const read of reads) {
    const returned = read.getType().compilerType;
    const body = checker.getAwaitedType(returned) ?? returned;
    if (!(body.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown))) {
      // The source's own client states what the body is; that is the
      // consumer's contract, and it is compared as one, not retyped.
      return (
        `the consumer's body read already states a type ` +
        `('${checker.typeToString(body)}'), so it is not retyped`
      );
    }
    const typed = typedAround(read);
    if (typed) return `the consumer's body read already states a type (${typed}), so it is not retyped`;
  }
  if (reads.every((read) => resultIsDiscarded(read))) {
    return 'the consumer never reads the response body';
  }
  for (const read of reads) {
    const escape = resultEscapes(read, true);
    if (escape) return escape.replace('the response', 'the response body');
  }
  return reads;
}

/**
 * What the source wraps around the parsed body beyond the ONE cast the
 * retype replaces: a second cast (`as unknown as Order`), an angle-bracket
 * assertion, or a call it is handed to (`parse(await res.json())`). Each
 * states or launders the body's type where the retype cannot reach, so a
 * replaced inner type would agree with anything.
 */
function typedAround(read: CallExpression): string | undefined {
  let node: Node = read;
  let cast = false;
  for (let parent = node.getParent(); parent; node = parent, parent = parent.getParent()) {
    if (Node.isAwaitExpression(parent) || Node.isParenthesizedExpression(parent)) continue;
    if (Node.isAsExpression(parent) && !cast) {
      cast = true;
      continue;
    }
    if (Node.isAsExpression(parent)) return 'a second cast';
    if (Node.isTypeAssertion(parent)) return 'a type assertion';
    if (
      (Node.isCallExpression(parent) || Node.isNewExpression(parent)) &&
      parent.getArguments().includes(node)
    ) {
      return 'a call it is handed to';
    }
    return undefined;
  }
  return undefined;
}

/** The names the read's parsed body is bound to, if it is bound. */
function resultNames(read: CallExpression): Identifier[] {
  let top: Node = read;
  for (let parent = top.getParent(); parent; parent = top.getParent()) {
    if (
      !Node.isAwaitExpression(parent) &&
      !Node.isParenthesizedExpression(parent) &&
      !Node.isAsExpression(parent)
    ) {
      break;
    }
    top = parent;
  }
  const declaration = top.getParent();
  if (!declaration || !Node.isVariableDeclaration(declaration) || declaration.getInitializer() !== top) {
    return [];
  }
  const name = declaration.getNameNode();
  return Node.isIdentifier(name)
    ? [name]
    : name.getDescendantsOfKind(SyntaxKind.Identifier).filter((id): id is Identifier => {
        const parent = id.getParent();
        return Node.isBindingElement(parent) && parent.getNameNode() === id;
      });
}

/** Whether a node names the response the read is on. */
function responseTest(read: CallExpression): (node: Node) => boolean {
  const receiver = (read.getExpression() as PropertyAccessExpression).getExpression();
  const symbol = Node.isIdentifier(receiver) ? receiver.getSymbol() : undefined;
  if (!symbol) return () => false;
  return (node) => Node.isIdentifier(node) && node.getSymbol() === symbol;
}

type Side = 'success' | 'failure' | 'unclear';

/**
 * Which side of a test of the response's status `node` runs on: inside a
 * branch of one, or after an `if (test) return/throw` that leaves the rest of
 * the block to the other side. `undefined` when no test decides it.
 */
function sideOf(node: Node, isResponse: (node: Node) => boolean): Side | undefined {
  let found: Side | undefined;
  const note = (side: Side | undefined) => {
    if (side === 'failure' || found === 'failure') found = 'failure';
    else if (side === 'unclear' || found === 'unclear') found = 'unclear';
    else found = side ?? found;
  };
  for (let child: Node = node, parent = node.getParent(); parent; child = parent, parent = parent.getParent()) {
    if (Node.isIfStatement(parent) && child !== parent.getExpression()) {
      note(branchSide(parent.getExpression(), child === parent.getThenStatement(), isResponse));
    } else if (Node.isConditionalExpression(parent) && child !== parent.getCondition()) {
      note(branchSide(parent.getCondition(), child === parent.getWhenTrue(), isResponse));
    } else if (Node.isCaseClause(parent) || Node.isDefaultClause(parent)) {
      const swtch = parent.getParent()?.getParent();
      if (swtch && Node.isSwitchStatement(swtch) && testsResponse(swtch.getExpression(), isResponse)) {
        note('unclear');
      }
    }
    if (Node.isBlock(parent) || Node.isSourceFile(parent) || Node.isCaseClause(parent)) {
      for (const statement of parent.getStatements()) {
        if (statement === child) break;
        if (
          Node.isIfStatement(statement) &&
          !statement.getElseStatement() &&
          exits(statement.getThenStatement())
        ) {
          note(branchSide(statement.getExpression(), false, isResponse));
        }
      }
    }
  }
  return found;
}

function branchSide(
  condition: Node,
  whenTrue: boolean,
  isResponse: (node: Node) => boolean
): Side | undefined {
  const ok = okWhenTrue(condition, isResponse);
  if (ok === undefined || ok === 'unclear') return ok;
  return ok === whenTrue ? 'success' : 'failure';
}

/**
 * Whether `condition` being true means the response succeeded: `res.ok`,
 * `res.status === 200`, `res.status !== 200`, `res.status >= 400` and their
 * negations. Any other test of the response is `'unclear'`; a condition that does not
 * test the response is `undefined`.
 */
function okWhenTrue(condition: Node, isResponse: (node: Node) => boolean): boolean | 'unclear' | undefined {
  let e = condition;
  while (Node.isParenthesizedExpression(e)) e = e.getExpression();
  if (Node.isPrefixUnaryExpression(e) && e.getOperatorToken() === SyntaxKind.ExclamationToken) {
    const inner = okWhenTrue(e.getOperand(), isResponse);
    return typeof inner === 'boolean' ? !inner : inner;
  }
  if (isMember(e, 'ok', isResponse)) return true;
  if (Node.isBinaryExpression(e)) {
    const op = e.getOperatorToken().getKind();
    const [left, right] = [e.getLeft(), e.getRight()];
    const value =
      isMember(left, 'status', isResponse) && Node.isNumericLiteral(right)
        ? right.getLiteralValue()
        : undefined;
    if (value !== undefined) {
      const success = value >= 200 && value < 300;
      switch (op) {
        case SyntaxKind.EqualsEqualsEqualsToken:
        case SyntaxKind.EqualsEqualsToken:
          return success;
        case SyntaxKind.ExclamationEqualsEqualsToken:
        case SyntaxKind.ExclamationEqualsToken:
          return !success;
        case SyntaxKind.GreaterThanEqualsToken:
          if (value >= 300) return false;
          break;
      }
      return 'unclear';
    }
  }
  return testsResponse(e, isResponse) ? 'unclear' : undefined;
}

function isMember(node: Node, name: string, isResponse: (node: Node) => boolean): boolean {
  return (
    Node.isPropertyAccessExpression(node) && node.getName() === name && isResponse(node.getExpression())
  );
}

/** The expression reads the response's `ok` or `status`. */
function testsResponse(node: Node, isResponse: (node: Node) => boolean): boolean {
  return [node, ...node.getDescendantsOfKind(SyntaxKind.PropertyAccessExpression)].some(
    (n) => isMember(n, 'ok', isResponse) || isMember(n, 'status', isResponse)
  );
}

/** A statement that always leaves the function. */
function exits(statement: Node): boolean {
  if (Node.isReturnStatement(statement) || Node.isThrowStatement(statement)) return true;
  if (Node.isBlock(statement)) {
    const last = statement.getStatements().at(-1);
    return !!last && exits(last);
  }
  return false;
}

/** `node.json()` with no arguments, where `node` is the receiver. */
function bodyReadOn(node: Node): CallExpression | undefined {
  const access = node.getParent();
  if (
    !access ||
    !Node.isPropertyAccessExpression(access) ||
    access.getExpression() !== node ||
    access.getName() !== 'json'
  ) {
    return undefined;
  }
  const read = access.getParent();
  return read && Node.isCallExpression(read) && read.getExpression() === access && isBodyRead(read)
    ? read
    : undefined;
}

function isBodyRead(call: CallExpression): boolean {
  const callee = call.getExpression();
  return (
    Node.isPropertyAccessExpression(callee) &&
    callee.getName() === 'json' &&
    call.getArguments().length === 0
  );
}

function isAssignmentOperator(kind: SyntaxKind): boolean {
  return kind >= SyntaxKind.FirstAssignment && kind <= SyntaxKind.LastAssignment;
}

/**
 * The edits that state `P` at one body read. A cast the source wrote around
 * the parsed body (`(await res.json()) as Order`) is REPLACED, as a type
 * argument the source wrote is: what is judged is how the source uses the
 * body, not what it claimed the body was. Otherwise the read itself is cast:
 * `(res.json() as Promise<P>)`.
 */
function bodyReadEdit(read: CallExpression): (stated: string) => Edit[] {
  let node: Node = read;
  let awaited = false;
  for (let parent = node.getParent(); parent; parent = node.getParent()) {
    if (Node.isAwaitExpression(parent)) awaited = true;
    else if (Node.isAsExpression(parent)) {
      const type = parent.getTypeNodeOrThrow();
      const start = type.getStart();
      const end = type.getEnd();
      return (stated) => [
        awaited
          ? { start, end, text: stated, statedAt: 0 }
          : { start, end, text: `Promise<${stated}>`, statedAt: 'Promise<'.length },
      ];
    } else if (!Node.isParenthesizedExpression(parent)) break;
    node = parent;
  }
  const start = read.getStart();
  const end = read.getEnd();
  return (stated) => [
    // The compiler reports past a parenthesis, so a finding on the cast read
    // lands on `res`, an original position, never on this one.
    { start, end: start, text: '(' },
    { start: end, end, text: ` as Promise<${stated}>)`, statedAt: ' as Promise<'.length },
  ];
}

function declaresTypeParameters(call: CallExpression): boolean {
  return (resolvedDeclaration(call)?.typeParameters?.length ?? 0) > 0;
}

function fileDiagnostics(sourceFile: SourceFile): Diag[] {
  const program = sourceFile.getProject().getProgram().compilerObject;
  const node = sourceFile.compilerNode;
  return [...program.getSyntacticDiagnostics(node), ...program.getSemanticDiagnostics(node)]
    .filter((d) => d.file === node && d.start !== undefined)
    .map((d) => ({ start: d.start!, code: d.code, message: flatten(d.messageText) }));
}

function flatten(text: string | ts.DiagnosticMessageChain): string {
  return ts.flattenDiagnosticMessageText(text, ' ');
}

function key(d: Diag): string {
  return `${d.start}:${d.code}`;
}

function applyEdits(text: string, edits: Edit[]): string {
  let out = '';
  let cursor = 0;
  for (const edit of [...edits].sort((a, b) => a.start - b.start)) {
    out += text.slice(cursor, edit.start) + edit.text;
    cursor = edit.end;
  }
  return out + text.slice(cursor);
}

/** Map a position in the rewritten text back to the original, or mark it ours. */
function mapBack(pos: number, edits: Edit[]): Origin {
  let shift = 0;
  for (const edit of [...edits].sort((a, b) => a.start - b.start)) {
    const newStart = edit.start + shift;
    const newEnd = newStart + edit.text.length;
    if (pos < newStart) break;
    if (pos < newEnd) return { kind: 'inserted' };
    shift += edit.text.length - (edit.end - edit.start);
  }
  return { kind: 'original', pos: pos - shift };
}

/** 1-based line of an original-file position. */
function lineIndex(text: string): (pos: number) => number {
  const starts = [0];
  for (let i = 0; i < text.length; i++) if (text[i] === '\n') starts.push(i + 1);
  return (pos) => {
    let lo = 0;
    let hi = starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (starts[mid] <= pos) lo = mid;
      else hi = mid - 1;
    }
    return lo + 1;
  };
}
