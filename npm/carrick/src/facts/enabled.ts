// The off-switch for the fact file: its local reads (`lines.ts`) and its
// fetch (`fetch.ts`). On by default; off only when CARRICK_FACT_LINES is
// exactly "0". With it off no hook reads, fetches or writes anything under
// `.carrick/facts/`.

/** True unless `CARRICK_FACT_LINES` is exactly `"0"`; unset, empty or any other value is on. */
export function factsEnabled(env: NodeJS.ProcessEnv = process.env): boolean {
  return env["CARRICK_FACT_LINES"] !== "0";
}
