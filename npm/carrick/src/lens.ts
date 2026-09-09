// Code lenses: the one place a person sees what Carrick knows without asking.
//
// carrick#880, and the design record of 2026-09-09, candidate 1. Humans only:
// an agent cannot invoke a lens, so nothing here reaches the agent channel and
// the whole of it is judged as an editor surface.
//
// ZERO SUPPRESSION is the rule the rest of this file exists to keep. A laptop
// index holds facts only, so a route with no indexed consumer is the ordinary
// state and not a finding: a lens reading "0 consumers" would say "nobody calls
// this" where it means "not indexed here". So a lens is rendered only where the
// index holds at least one counterpart or one non-compatible verdict for that
// row, and the boundary answers the question of why the others are bare. The
// general form of the rule, from the same record: nothing states a number it
// cannot attribute.
//
// A candidate row produces no lens at all. Candidates are the model's reading
// and cannot reach a laptop index today; when they can, they are counted in a
// clause of their own and never inside the mismatch count, because the mismatch
// count is the number a person acts on.
//
// The lens command carries the boundary in its arguments rather than in a
// tooltip: `vscode-languageclient`'s `asCommand` copies title, command and
// arguments and drops everything else, so a tooltip set here would never reach
// a VS Code user. The extension shows those lines when the lens is clicked, and
// the status bar item from carrick#879 is where the boundary lives for a person
// the rest of the time.

import {
  isCandidate,
  type CheckItem,
  type CheckResult,
  type Counterpart,
} from "./contract.ts";
import { boundaryFor } from "./render.ts";
import { rangeAt, resolveCounterpart } from "./diagnostics.ts";
import { DEFAULT_SURFACES, type Surfaces } from "./surfaces.ts";

/** The command a client invokes when a lens is clicked. */
export const SHOW_COUNTERPARTS = "carrick.showCounterparts";

export type LensCommand = {
  title: string;
  command: string;
  arguments?: unknown[];
};

export type CodeLens = {
  range: ReturnType<typeof rangeAt>;
  command?: LensCommand;
};

/** What the command is handed: enough to list the other side and open it. */
export type CounterpartList = {
  /** The row's own operation, e.g. `GET /api/users/:id`. */
  operation: string;
  /** Absolute paths, already checked to exist, one per site. */
  sites: Array<{
    role: string;
    service: string | null;
    /** Absolute; null when the counterpart is not on this disk. */
    path: string | null;
    line: number | null;
  }>;
  /** The service's boundary lines, so an empty list reads correctly. */
  boundary: string[];
};

export type LensOptions = {
  /** Injectable for tests. */
  exists?: (target: string) => boolean;
  surfaces?: Surfaces;
};

/** A verdict that states a result other than `compatible`, on this one row. */
function isMismatch(item: CheckItem): boolean {
  const verdict = item.verdict;
  return Boolean(verdict && verdict.result != null && verdict.result !== "compatible");
}

/**
 * How many of the other side there are, in the word for what they are.
 *
 * One role gets its own noun; a mixed row gets the neutral one. Nothing here
 * can render a zero: the caller has already decided there is something to say.
 */
function counterpartClause(counterparts: Counterpart[]): string {
  const roles = new Set(counterparts.map((counterpart) => counterpart.role));
  const count = counterparts.length;
  const plural = count === 1 ? "" : "s";
  if (roles.size === 1 && roles.has("consumer")) return `${count} consumer${plural}`;
  if (roles.size === 1 && roles.has("producer")) return `${count} producer${plural}`;
  if (roles.size === 1 && roles.has("peer")) return `${count} service${plural} on the same contract`;
  return `${count} counterpart${plural}`;
}

function operationOf(item: CheckItem): string {
  return [item.method, item.path].filter(Boolean).join(" ") || item.kind;
}

/**
 * One lens per row that the index knows something actionable about.
 *
 * Never one per line of a handler, never one for a row whose line the payload
 * did not state, and never one that would have to say zero.
 */
export function toCodeLenses(
  result: CheckResult,
  options: LensOptions = {},
): CodeLens[] {
  const surfaces = options.surfaces ?? DEFAULT_SURFACES;
  if (!surfaces.codeLens) return [];
  if (result.error) return [];

  const exists = options.exists;
  // `carrick.boundary` off takes the boundary off this surface too, or the
  // switch would only half work.
  const boundary = surfaces.boundary ? boundaryFor(result) : [];
  const lenses: CodeLens[] = [];
  for (const item of result.items ?? []) {
    // A row the model alone states informs a pull surface and asserts nothing
    // on a pushed one. A lens is rendered without being asked for, so no.
    if (isCandidate(item)) continue;
    if (typeof item.line !== "number") continue;
    const counterparts = item.counterparts ?? [];
    const mismatch = isMismatch(item);
    if (counterparts.length === 0 && !mismatch) continue;

    const clauses: string[] = [];
    if (counterparts.length > 0) clauses.push(counterpartClause(counterparts));
    // One row carries one verdict, so the count is one or none. It is stated as
    // a count anyway, because it is the number a person acts on and the clause
    // has to stay countable when candidates gain one of their own.
    if (mismatch) clauses.push("1 mismatch");

    const payload: CounterpartList = {
      operation: operationOf(item),
      sites: counterparts.map((counterpart) => ({
        role: counterpart.role,
        service: counterpart.service ?? null,
        path: resolveCounterpart(counterpart, exists),
        line: counterpart.line ?? null,
      })),
      boundary,
    };
    lenses.push({
      range: rangeAt(item.line, item.col),
      command: {
        title: clauses.join(", "),
        command: SHOW_COUNTERPARTS,
        arguments: [payload],
      },
    });
  }
  return lenses;
}
