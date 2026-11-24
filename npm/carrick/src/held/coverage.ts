// `carrick doctor`'s account of the compiler check (T7, #4654): per service,
// how many rows are checked at edit time, the rest by reason with counts, and
// the engine's state. Read from the files the daemon keeps beside each
// service's placed file; nothing here starts an engine.

import fs from "node:fs";
import path from "node:path";
import { readJson } from "../hook/lock.ts";
import { serviceFilesDir } from "./paths.ts";
import type { ServiceState } from "./state.ts";

type Fit = { strictNullChecks?: boolean; strictFunctionTypes?: boolean; exactOptionalPropertyTypes?: boolean };
type Placed = {
  service?: string;
  armed?: unknown[];
  unarmed?: Array<{ reason?: string }>;
  /** Written with `options_differ`: both sides' values and the tsconfig the service's came from. */
  options?: { service?: Fit; judge?: Fit };
  tsconfig?: string;
};
type Held = { placed: Placed | null; state: ServiceState | null };

/** An unarmed reason as the phrase a person reads after "N rows". */
function reasonText(reason: string, placed: Placed): string {
  const failing = /^clean_check_fails:(TS\d+)$/.exec(reason);
  if (failing) return `already reporting ${failing[1]} before the check runs`;
  switch (reason) {
    case "no_values":
      return "with no stored send position";
    case "options_differ":
      return differText(placed);
    case "twin_passes":
      return "that would not fail the check on a deliberately wrong value";
    case "decl_error":
      return "comparing a type with a compile error";
    case "value_edited":
      return "with a send position that cannot be checked as written";
    case "top_type":
      return "with no checkable type";
    case "shared_home":
      return "whose caller type is declared in a file this service also compiles";
    case "field_resolver_unread":
      return "whose edited value reaches the caller through a field resolver, which Carrick does not read";
    case "javascript_file":
      return "in a JavaScript file";
    case "graphql_table_unusable":
      return "needing a GraphQL type table that could not be used";
    case "message_rules_missing":
      return "needing message rules this service does not have";
    default:
      return `not checked (${reason})`;
  }
}

const FIT_OPTIONS = ["strictNullChecks", "strictFunctionTypes", "exactOptionalPropertyTypes"] as const;

/** `options_differ`, naming the options and both values when the placed file kept them (an older file did not). */
function differText(placed: Placed): string {
  const own = placed.options?.service;
  const judge = placed.options?.judge;
  const named = own === undefined || judge === undefined ? [] : FIT_OPTIONS.filter((key) => own[key] !== judge[key]);
  if (own === undefined || judge === undefined || named.length === 0) return "in a service whose compiler options differ from Carrick's";
  const values = named.map((key) => `${key} is ${own[key] ? "on" : "off"} here and ${judge[key] ? "on" : "off"} in Carrick's check`).join("; ");
  return `in a service whose TypeScript options differ from the ones Carrick compares under: ${values}. The service's values come from ${placed.tsconfig ?? "its tsconfig"}`;
}

function engineText(state: ServiceState): string {
  switch (state.state) {
    case "ready":
      return state.start_ms === undefined ? "The compiler engine is ready." : `The compiler engine is ready (started in ${Math.round(state.start_ms / 1000)} s).`;
    case "starting":
      return "The compiler engine is starting.";
    case "capped":
      return `The compiler engine did not start: the ${state.budget_mb ?? 0} MB memory budget is held by ${state.holders ?? 0} other engines.`;
    case "no_compiler":
      return "The compiler engine did not start: no TypeScript 7 compiler is installed.";
    case "failed":
      return `The compiler engine could not start${state.reason === undefined ? "" : `: ${state.reason.replace(/[.\s]+$/, "")}`}.`;
  }
}

/** More services than this with no typed route are one line with a count. */
const MAX_NOTHING_LINES = 5;

type Filed = Held & { repo: string | null };

/** The records the daemon holds for one checkout, by service name. */
function recorded(root: string, home: string | undefined): Map<string, Filed> {
  const found = new Map<string, Filed>();
  const dir = serviceFilesDir(root, home);
  let names: string[] = [];
  try {
    names = fs.readdirSync(dir);
  } catch {
    return found;
  }
  for (const name of names) {
    const kind = name.endsWith(".placed.json") ? "placed" : name.endsWith(".state.json") ? "state" : null;
    if (kind === null) continue;
    const body = readJson<Placed & ServiceState>(path.join(dir, name));
    if (body === null || typeof body.service !== "string") continue;
    const entry = found.get(body.service) ?? { placed: null, state: null, repo: null };
    if (kind === "placed") entry.placed = body;
    else {
      entry.state = body;
      // The placed file names no repo; the state record does.
      entry.repo = typeof body.repo === "string" ? body.repo : null;
    }
    found.set(body.service, entry);
  }
  return found;
}

/** No engine has started here, and the fact file holds no response row for the service: none ever will. */
const NO_TYPED_ROUTE = "no typed route to check.";

function serviceLine(head: string, { placed, state }: Held, typed: boolean | null = null): string {
  const engine = state === null ? "" : ` ${engineText(state)}`;
  if (placed === null && state === null && typed === false) return `${head} ${NO_TYPED_ROUTE}`;
  if (placed === null) return `${head} ${state === null ? "not started yet." : "nothing is checked yet."}${engine}`;
  const armed = placed.armed?.length ?? 0;
  const unarmed = placed.unarmed ?? [];
  const total = armed + unarmed.length;
  if (total === 0) return `${head} no route with a stored send position.${engine}`;
  const counts = new Map<string, number>();
  for (const row of unarmed) counts.set(row.reason ?? "unknown", (counts.get(row.reason ?? "unknown") ?? 0) + 1);
  const rest = [...counts].map(([reason, n]) => `${n} ${n === 1 ? "row" : "rows"} ${reasonText(reason, placed)}`);
  return `${head} ${armed} of ${total} rows are checked after an edit.${rest.length > 0 ? ` Not checked: ${rest.join("; ")}.` : ""}${engine}`;
}

/**
 * One line per service. The daemon files its records under the workspace root,
 * so they are read once from there and each goes under the repo its state
 * record names (a record naming none goes under the workspace). Declared
 * services are per repo. `on` is the compiler switch; with it off and nothing
 * ever recorded there is nothing to say.
 */
export function compilerCoverage(
  workspace: { root: string; label: string; repos: ReadonlyArray<{ name: string; label: string; services: readonly string[] }> },
  on: boolean,
  home?: string,
  /** The services that hold a response row in the fact file; null when it cannot be read (nothing is then said about "no typed route"). */
  typed: ReadonlySet<string> | null = null,
): string[] {
  const held = recorded(workspace.root, home);
  if (!on && held.size === 0) return [];
  const none: Held = { placed: null, state: null };
  const mine = new Set<string>();
  const lines: string[] = [];
  // Services with nothing to check are one line when there are many.
  const nothing: Array<{ label: string; name: string }> = [];
  const emit = (label: string, services: Iterable<string>): void => {
    for (const name of services) {
      const mineHeld = held.get(name) ?? none;
      if (mineHeld.placed === null && mineHeld.state === null && typed !== null && !typed.has(name)) nothing.push({ label, name });
      else lines.push(serviceLine(`${label}: compiler check, service "${name}":`, mineHeld, typed === null ? null : typed.has(name)));
    }
  };
  for (const repo of workspace.repos) {
    const recordedHere = [...held].filter(([, r]) => r.repo === repo.name).map(([name]) => name);
    const names = [...new Set([...repo.services, ...recordedHere])];
    // A declared service another repo's record already claims is that repo's.
    const own = names.filter((name) => held.get(name)?.repo === undefined || held.get(name)?.repo === null || held.get(name)?.repo === repo.name);
    for (const name of own) mine.add(name);
    emit(repo.label, own);
  }
  emit(workspace.label, [...held.keys()].filter((name) => !mine.has(name)));
  if (nothing.length > MAX_NOTHING_LINES) {
    const first = nothing.slice(0, 3).map((entry) => entry.name).join(", ");
    lines.push(`${workspace.label}: compiler check, ${NO_TYPED_ROUTE.replace(/\.$/, "")} in ${nothing.length} services (${first} and ${nothing.length - 3} more).`);
  } else for (const { label, name } of nothing) lines.push(`${label}: compiler check, service "${name}": ${NO_TYPED_ROUTE}`);
  return lines;
}
