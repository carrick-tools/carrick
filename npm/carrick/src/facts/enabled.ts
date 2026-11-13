// The off-switch for the fact file: its local reads (`lines.ts`) and its
// fetch (`fetch.ts`). On by default; off only when CARRICK_FACT_LINES is
// exactly "0". With it off no hook reads, fetches or writes anything under
// `.carrick/facts/`.

/** True unless `CARRICK_FACT_LINES` is exactly `"0"`; unset, empty or any other value is on. */
export function factsEnabled(env: NodeJS.ProcessEnv = process.env): boolean {
  return env["CARRICK_FACT_LINES"] !== "0";
}

/**
 * The contract line's switch: opt-in, on only when `CARRICK_CONTRACT_LINES` is
 * exactly `"1"` and fact lines are on. Contract rows are stored either way;
 * this decides only whether a line is printed from them (and, in `fetch.ts`,
 * whether a file without the `contracts` table is replaced).
 */
export function contractLinesEnabled(env: NodeJS.ProcessEnv = process.env): boolean {
  return env["CARRICK_CONTRACT_LINES"] === "1" && factsEnabled(env);
}
