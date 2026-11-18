// The held compiler's early answer (part 3, B1): the breaks the daemon found for
// one queued edit, as a `CheckResult` built from the fact row, so it prints
// through the one renderer (`renderPostToolUse`) and compares row by row with
// the full re-check's answer (`hook/confirm.ts`).
//
// A row in the edited file is a `CheckItem`; a row in another file is a
// `recheck.moved` row. The compiler's message is the verdict's detail, with the
// check's own alias replaced; a message that still names the check's machinery
// is never written. Every row is `stated`: the daemon checks only a row its
// gate armed, whose clean check was silent before the edit (`held/place.ts`,
// `arm`), so each break is this edit's. The full answer's rows carry the
// CLI's own mark.

import path from "node:path";
import type { CheckItem, CheckResult, Counterpart, MovedRow } from "../contract.ts";
import { realPath } from "../fs-util.ts";
import type { HeldLine } from "./daemon.ts";

/** A type longer than this is cut to its start, each side on its own. */
const MAX_SIDE = 60;
/** The check's alias in a message: `O<row>_<group>`. */
const ALIAS = /'O\d+_\d+'/g;
/** A quoted inline object type; its inner text has no quote of its own (the compiler quotes literals with double quotes). */
const INLINE_OBJECT = /'\{[^']*\}'/g;
/** What must never reach an agent. */
const MACHINERY = /__C\b|\bsend\d|\bO\d+_\d+\b|__carrick_/;
/** At most this many characters in one detail. */
export const MAX_HELD_DETAIL = 240;
/** What the expected type is cut to, as the CLI cuts its own. */
const MAX_TYPE = 200;

/** The type a message names for the check's own alias. */
function expectedName(line: Pick<HeldLine, "symbol"> & { expected?: string }): string {
  if (line.symbol) return `'${line.symbol}'`;
  const own = line.expected?.replace(/\s+/g, " ").trim();
  return own ? shortSide(`'${own.replace(/'/g, '"')}'`) : "a different type";
}

/** One quoted type in a message, cut to `MAX_SIDE` characters when it is longer. */
function shortSide(quoted: string): string {
  if (quoted.length <= MAX_SIDE) return quoted;
  const inner = quoted.startsWith("'") && quoted.endsWith("'") ? quoted.slice(1, -1) : quoted;
  const open = inner.startsWith("{");
  return `'${inner.slice(0, MAX_SIDE - 6).trimEnd()} …${open ? " }" : ""}'`;
}

const TYPE = "('[^']*'|a different type)";
const PROP_MISSING = new RegExp(`Property '([^']+)' is missing in type ${TYPE} but required in type ${TYPE}`);
const NOT_ASSIGNABLE = new RegExp(`^Type ${TYPE} is not assignable to type ${TYPE}\\.?$`);
const PROP_PATH = /^Types of property '([^']+)' are incompatible\.?$/;

/**
 * The voice table (HM4 D4): who sends and who reads, by which end of the check
 * this code is. The sentences are built from these two subjects in one place.
 * In the compiler's "Type S is not assignable to type E", S is always the
 * "sends" slot and E the "expects" slot; only the subjects move.
 */
type Voice = { sender: string; reader: string };
const PRODUCER_VOICE: Voice = { sender: "This code", reader: "the caller" };
const SOCKET_EMIT_VOICE: Voice = { sender: "This code", reader: "the listener" };
const HTTP_CALL_VOICE: Voice = { sender: "The route", reader: "this code" };

/** The voice of one line, or null for a consumer combination with no row yet. */
function voiceOf(line: Pick<HeldLine, "side" | "reads" | "protocol">): Voice | null {
  if (line.side !== "consumer") return PRODUCER_VOICE;
  if (line.reads !== true && line.protocol === "socket") return SOCKET_EMIT_VOICE;
  if (line.reads === true && line.protocol === "http") return HTTP_CALL_VOICE;
  return null;
}

/** The detail's wording for the three kinds of leaf the compiler gives. */
function missingDetail(v: Voice, prop: string, sent: string, expected: string): string {
  return `'${prop}' is missing. ${v.sender} sends ${sent}; ${v.reader} expects ${expected}.`;
}
function differsDetail(v: Voice, prop: string, sent: string, expected: string): string {
  return `'${prop}' has a different type. ${v.sender} sends ${sent}; ${v.reader} expects ${expected}.`;
}
function wholeDetail(v: Voice, sent: string, expected: string): string {
  return `${v.sender} sends ${sent}; ${v.reader} expects ${expected}.`;
}

/**
 * The part of the compiler's chain that names the difference. The chain runs
 * from the whole types down to the leaf, so the leaf is the last line: a
 * missing property, or a property path ending in two types. Returns null when
 * the message is none of those.
 */
function leafDetail(message: string, v: Voice): string | null {
  const lines = message.split("\n").map((l) => l.replace(/\s+/g, " ").trim()).filter((l) => l !== "");
  const last = lines[lines.length - 1];
  if (last === undefined) return null;
  const missing = PROP_MISSING.exec(last);
  if (missing) {
    const path = lines.slice(0, -1).flatMap((l) => PROP_PATH.exec(l)?.[1] ?? []);
    return missingDetail(v, [...path, missing[1]!].join("."), shortSide(missing[2]!), shortSide(missing[3]!));
  }
  const path = lines.slice(0, -1).flatMap((l) => PROP_PATH.exec(l)?.[1] ?? []);
  const pair = NOT_ASSIGNABLE.exec(last);
  if (pair === null) return null;
  const [sent, expected] = [shortSide(pair[1]!), shortSide(pair[2]!)];
  return path.length > 0 ? differsDetail(v, path.join("."), sent, expected) : wholeDetail(v, sent, expected);
}

/** The compiler's message for one break, in words an agent may read, or null when it still shows the check's machinery. */
export function heldDetail(
  line: Pick<HeldLine, "message" | "symbol"> & Partial<Pick<HeldLine, "side" | "reads" | "protocol">> & { expected?: string },
): string | null {
  const voice = voiceOf(line);
  if (voice === null) return null;
  const aliased = line.message.replace(ALIAS, () => expectedName(line));
  const message = (leafDetail(aliased, voice) ?? aliased.replace(INLINE_OBJECT, (whole) => shortSide(whole)))
    .replace(/\s+/g, " ")
    .trim();
  if (MACHINERY.test(message)) return null;
  return message.length > MAX_HELD_DETAIL ? `${message.slice(0, MAX_HELD_DETAIL - 1)}…` : message;
}

/**
 * A contract row's label in the parts a check item carries: `METHOD path`, and
 * the dispatch case (` {field=value}`) when the label ends in one, so that
 * `method + " " + path + dispatch_label` is the label again.
 */
export function splitLabel(label: string): { method: string; path: string; dispatch_label?: string } {
  const space = label.indexOf(" ");
  const method = space < 0 ? label : label.slice(0, space);
  const rest = space < 0 ? "" : label.slice(space + 1);
  const dispatch = / \{[^{}]*\}$/.exec(rest);
  if (dispatch === null) return { method, path: rest };
  return { method, path: rest.slice(0, dispatch.index), dispatch_label: dispatch[0] };
}

/** A type as an item states it: cut at `MAX_TYPE`, as the CLI cuts its own. */
const cutType = (type: string): string => (type.length > MAX_TYPE ? `${type.slice(0, MAX_TYPE - 3)}...` : type);

/**
 * A consumer-side break (a socket emit; HM4 D1, moved with carrick#4770): a
 * `call` row at the call's current line, shaped as the full re-check shapes
 * the same row. On a message row the re-check states the emitter's payload as
 * `actual_type`, the listener's type as `expected_type` and `sent_by:
 * "consumer"` (`typed_sides`, `src/local_mode/join.rs`). The early row states
 * only the listener's held-fixed type: the emitter's own is the edited side.
 * Two calls of one event in one file are two rows: the key carries the line.
 */
function consumerRow(
  entry: EarlyFor,
  edited: string,
  line: HeldLine & { call_line: number },
  detail: string,
  parts: ReturnType<typeof splitLabel>,
  items: Map<string, CheckItem>,
  moved: Map<string, MovedRow>,
): void {
  if (realPath(line.row_path) === edited) {
    const counterparts: Counterpart[] = line.places.map((p) => ({ role: "producer", service: p.service, file: p.file, ...(p.line === null ? {} : { line: p.line }) }));
    const key = `${line.label}\0${line.call_line}`;
    const held = items.get(key);
    if (held) {
      held.counterparts = [...(held.counterparts ?? []), ...counterparts];
      return;
    }
    items.set(key, {
      kind: "call",
      ...parts,
      line: line.call_line,
      direction: "response",
      verdict: { state: "resolved", result: "type_mismatch", detail },
      expected_type: cutType(line.expected),
      sent_by: "consumer",
      counterparts,
      stated: true,
    });
    return;
  }
  const file = path.relative(realPath(entry.root), realPath(line.row_path));
  const places = line.places.map((p) => `${p.service} ${p.file}${p.line === null ? "" : `:${p.line}`}`);
  const key = `${file}\0${line.label}\0${line.call_line}`;
  const held = moved.get(key);
  if (held) {
    held.counterparts.push(...places);
    return;
  }
  moved.set(key, {
    file,
    line: line.call_line,
    role: "call",
    method: parts.method,
    path: parts.path + (parts.dispatch_label ?? ""),
    before: "compatible",
    after: "type_mismatch",
    counterparts: places,
    reason: detail,
    not_judged_before: false,
    stated: true,
  });
}

/** The entry an early answer is for: the edited file, absolute, and the workspace it was queued in. */
export type EarlyFor = { file: string; root: string };

/**
 * The early answer for one entry: a `CheckResult` whose items are the breaks in
 * the edited file and whose `recheck.moved` rows are the breaks in other files.
 * Each (row, group) the daemon names is one row; a row named twice (two
 * groups) is one row with every group's places.
 */
export function earlyResult(entry: EarlyFor, lines: readonly HeldLine[]): CheckResult {
  const edited = realPath(entry.file);
  const items = new Map<string, CheckItem>();
  const moved = new Map<string, MovedRow>();
  for (const line of lines) {
    // A consumer line part 2 cannot be joined to (no call line) is not printed: the full check could neither confirm nor correct it.
    if (line.side === "consumer" && typeof line.call_line !== "number") continue;
    const detail = heldDetail(line);
    if (detail === null) continue;
    const parts = splitLabel(line.label);
    if (line.side === "consumer") {
      consumerRow(entry, edited, line as HeldLine & { call_line: number }, detail, parts, items, moved);
      continue;
    }
    if (realPath(line.row_path) === edited) {
      const counterparts: Counterpart[] = line.places.map((p) => ({
        role: "consumer",
        service: p.service,
        file: p.file,
        ...(p.line === null ? {} : { line: p.line }),
      }));
      const held = items.get(line.label);
      if (held) {
        held.counterparts = [...(held.counterparts ?? []), ...counterparts];
        continue;
      }
      items.set(line.label, {
        kind: "route",
        ...parts,
        ...(line.row_line === null ? {} : { line: line.row_line }),
        direction: "response",
        verdict: { state: "resolved", result: "type_mismatch", detail },
        expected_type: cutType(line.expected),
        counterparts,
        stated: true,
      });
      continue;
    }
    const file = path.relative(realPath(entry.root), realPath(line.row_path));
    const places = line.places.map((p) => `${p.service} ${p.file}${p.line === null ? "" : `:${p.line}`}`);
    const key = `${file}\0${line.label}`;
    const held = moved.get(key);
    if (held) {
      held.counterparts.push(...places);
      continue;
    }
    moved.set(key, {
      file,
      ...(line.row_line === null ? {} : { line: line.row_line }),
      role: "route",
      method: parts.method,
      path: parts.path + (parts.dispatch_label ?? ""),
      before: "compatible",
      after: "type_mismatch",
      counterparts: places,
      reason: detail,
      not_judged_before: false,
      stated: true,
    });
  }
  return {
    schema: "carrick.check/0",
    file: path.relative(realPath(entry.root), edited),
    items: [...items.values()],
    ...(moved.size > 0 ? { recheck: { ran: "extraction+types", elapsed_ms: 0, moved: [...moved.values()] } } : {}),
  };
}
