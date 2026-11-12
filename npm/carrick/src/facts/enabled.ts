// The off-switch for the fact file: its local reads (`lines.ts`) and its
// fetch (`fetch.ts`). Off unless CARRICK_FACT_LINES is exactly "1"; with it
// off no hook reads, fetches or writes anything under `.carrick/facts/`.

/** True only when `CARRICK_FACT_LINES` is exactly `"1"`. */
export function factsEnabled(env: NodeJS.ProcessEnv = process.env): boolean {
  return env["CARRICK_FACT_LINES"] === "1";
}
