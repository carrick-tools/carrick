// The held compiler's early answer (part 3, B1): the breaks the daemon found for
// one queued edit, as a `CheckResult` built from the fact row, so it prints
// through the one renderer (`renderPostToolUse`) and compares row by row with
// the full re-check's answer (`hook/confirm.ts`).
//
// A row in the edited file is a `CheckItem`; a row in another file is a
// `recheck.moved` row. The compiler's message is the verdict's detail, with the
// check's own alias replaced; a message that still names the check's machinery
// is never written.

import path from "node:path";
import type { CheckItem, CheckResult, Counterpart, MovedRow } from "../contract.ts";
import { realPath } from "../fs-util.ts";
import type { HeldLine } from "./daemon.ts";

/** A quoted inline object type longer than this becomes `the reply`. */
const MAX_INLINE_TYPE = 80;
/** The check's alias in a message: `O<row>_<group>`. */
const ALIAS = /'O\d+_\d+'/g;
/** A quoted inline object type; its inner text has no quote of its own (the compiler quotes literals with double quotes). */
const INLINE_OBJECT = /'(\{[^']*\})'/g;
/** What must never reach an agent. */
const MACHINERY = /__C\b|\bsend\d|\bO\d+_\d+\b|__carrick_/;
/** At most this many characters in one detail. */
export const MAX_HELD_DETAIL = 240;
/** What the expected type is cut to, as the CLI cuts its own. */
const MAX_TYPE = 200;

/** The compiler's message for one break, in words an agent may read, or null when it still shows the check's machinery. */
export function heldDetail(line: Pick<HeldLine, "message" | "symbol">): string | null {
  const message = line.message
    .replace(ALIAS, line.symbol ? `'${line.symbol}'` : "the type it expects")
    .replace(INLINE_OBJECT, (whole, inner: string) => (inner.length > MAX_INLINE_TYPE ? "the reply" : whole))
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
    const detail = heldDetail(line);
    if (detail === null) continue;
    const parts = splitLabel(line.label);
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
        expected_type: line.expected.length > MAX_TYPE ? `${line.expected.slice(0, MAX_TYPE - 3)}...` : line.expected,
        counterparts,
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
    });
  }
  return {
    schema: "carrick.check/0",
    file: path.relative(realPath(entry.root), edited),
    items: [...items.values()],
    ...(moved.size > 0 ? { recheck: { ran: "extraction+types", elapsed_ms: 0, moved: [...moved.values()] } } : {}),
  };
}
