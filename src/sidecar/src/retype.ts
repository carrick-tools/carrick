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
 * Runs in the consumer's REAL program (the init'd project), because that is
 * the only place its file can type-check: the check workspace holds
 * declaration stubs, not consumer sources, and none of the consumer's imports.
 *
 * Every edit is undone before the next item and before returning, so the
 * project other requests read is the project the scan loaded.
 */

import { Node, SyntaxKind, ts } from 'ts-morph';
import type { CallExpression, Project, SourceFile, Type } from 'ts-morph';
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
}

interface Diag {
  /** Start position in the file the diagnostic was reported on. */
  start: number;
  code: number;
  message: string;
}

/** Where a position in the rewritten text came from. */
type Origin = { kind: 'original'; pos: number } | { kind: 'inserted' };

interface Rewrite {
  form: 'type_argument' | 'cast';
  edits: Edit[];
  /** Start/end of the rewritten call in the NEW text, for re-locating it. */
  callStart: number;
  callEnd: number;
}

export class Retyper {
  /**
   * `jsonWire` is the check phase's own JSON wire transform
   * (`jsonWireDeclarations` in the capture bundle), handed in by the entry
   * point so both judges read the wire the same way without this file
   * crossing the bundle seam.
   */
  constructor(
    private readonly project: Project,
    private readonly inferrer: TypeInferrer,
    private readonly jsonWire: (prefix: string) => string[]
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

    const plan = this.plan(call, producer, item.wire);
    if (typeof plan === 'string') return abstain(item, plan);

    const original = sourceFile.getFullText();
    let pre = before.get(sourceFile);
    if (!pre) {
      pre = fileDiagnostics(sourceFile);
      before.set(sourceFile, pre);
    }
    const decisive = this.check(sourceFile, original, plan(true), pre);
    if (decisive.kind === 'abstain') return abstain(item, decisive.reason, decisive.form);
    if (decisive.added.length === 0) {
      return {
        item_id: item.item_id,
        outcome: 'agrees',
        form: decisive.form,
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
      form: decisive.form,
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
          form: 'type_argument',
          edits: [{ start: argStart, end: argEnd, text }],
          callStart,
          callEnd: callEnd + delta,
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
          form: 'type_argument',
          edits: [{ start: at, end: at, text }],
          callStart,
          callEnd: callEnd + text.length,
        };
      };
    }

    // No type parameter to state. A call to a function the program declares
    // whose result it types as `any` (an untyped helper) can still be asked to
    // return the producer's type with a cast; a call that returns a typed
    // value (a `Response` whose body is read later) cannot, and is left alone.
    //
    // A callee that is itself `any` is neither: it is a client the program
    // could not resolve (a checkout without its dependencies), and what it
    // returns may be an envelope around the payload, so casting it to the
    // payload would report the envelope's own members as missing.
    if (!resolvedDeclaration(call)) {
      return 'the consumer call does not resolve in its program (is the client installed?)';
    }
    const result = call.getType();
    const promised = promiseArgument(result);
    if (!result.isAny() && !(promised && promised.isAny())) {
      return (
        `the consumer call takes no type argument and returns ` +
        `'${result.getText(call)}', so it cannot be retyped`
      );
    }
    return (useWire) => {
      const inner = stated(useWire);
      const target = promised ? `Promise<${inner}>` : inner;
      return {
        form: 'cast',
        edits: [
          { start: callStart, end: callStart, text: '(' },
          { start: callEnd, end: callEnd, text: ` as ${target})` },
        ],
        callStart: callStart + 1,
        callEnd: callEnd + 1,
      };
    };
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
    | { kind: 'checked'; form: Rewrite['form']; added: Diag[] }
    | { kind: 'abstain'; form: Rewrite['form']; reason: string } {
    const appendix =
      '\n' +
      [
        ...this.jsonWire(PREFIX),
        `type ${WIRE}<P> = ${PREFIX}JsonWireSame<${PREFIX}JsonWire<P>, P> extends true ? P : ${PREFIX}JsonWire<P>;`,
      ].join('\n') +
      '\n';
    const rewritten = applyEdits(original, rewrite.edits) + appendix;
    const bodyEnd = rewritten.length - appendix.length;
    const originOf = (pos: number): Origin =>
      pos >= bodyEnd ? { kind: 'inserted' } : mapBack(pos, rewrite.edits);

    sourceFile.replaceWithText(rewritten);
    try {
      if (rewrite.form === 'type_argument') {
        const reached = this.typeArgumentReachesResult(sourceFile, rewrite);
        if (reached !== true) {
          return { kind: 'abstain', form: rewrite.form, reason: reached };
        }
      }

      const post = fileDiagnostics(sourceFile);
      const inserted = post.filter((d) => originOf(d.start).kind === 'inserted');
      if (inserted.length > 0) {
        return {
          kind: 'abstain',
          form: rewrite.form,
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
      return { kind: 'checked', form: rewrite.form, added };
    } finally {
      sourceFile.replaceWithText(original);
    }
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
    rewrite: Rewrite
  ): true | string {
    const call = sourceFile
      .getDescendantsOfKind(SyntaxKind.CallExpression)
      .find((c) => c.getStart() === rewrite.callStart && c.getEnd() === rewrite.callEnd);
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

function abstain(
  item: RetypeItem,
  reason: string,
  form?: RetypeOutcome['form']
): RetypeOutcome {
  return { item_id: item.item_id, outcome: 'abstain', form, diagnostics: [], reason };
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

function declaresTypeParameters(call: CallExpression): boolean {
  return (resolvedDeclaration(call)?.typeParameters?.length ?? 0) > 0;
}

function promiseArgument(type: Type): Type | undefined {
  const symbol = type.getSymbol() ?? type.getAliasSymbol();
  if (symbol?.getName() !== 'Promise') return undefined;
  return type.getTypeArguments()[0];
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
