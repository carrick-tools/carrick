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

/**
 * The chain line's switch: opt-in, on only when `CARRICK_CHAIN_LINES` is
 * exactly `"1"` and the contract line is on (a chain line ends in a contract
 * row's words). Chain rows are stored either way; this decides only whether a
 * line is printed from them (and, in `fetch.ts`, whether a file without the
 * `chains` table is replaced).
 */
export function chainLinesEnabled(env: NodeJS.ProcessEnv = process.env): boolean {
  return env["CARRICK_CHAIN_LINES"] === "1" && contractLinesEnabled(env);
}

/**
 * The Read's switch: opt-in, on only when `CARRICK_READ_LINES` is exactly
 * `"1"` and fact lines are on. Off, a Read (and a shell command that reads)
 * takes the binary's `check` answer and never opens the fact file
 * (`hook/read-answer.ts`).
 */
export function readLinesEnabled(env: NodeJS.ProcessEnv = process.env): boolean {
  return env["CARRICK_READ_LINES"] === "1" && factsEnabled(env);
}
