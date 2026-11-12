/**
 * v2 check core — abnormal tsc termination (adversarial-review finding 3).
 *
 * runCheck used to ignore the tsc exit code entirely: an OOM/SIGKILL
 * (code null), an internal crash, or a fileless config error (TS18003
 * prints WITHOUT a `(line,col)` prefix, so parseTscOutput yields zero
 * diagnostics) all fell through classifyPair's empty-diagnostics branch and
 * every pair read COMPATIBLE with success:true — the dangerous direction.
 *
 * These tests inject a stand-in tsc via CheckOptions.tscPath (the same
 * injection seam as pnpmPath) to pin:
 *  1. non-zero exit + zero parseable diagnostics -> every pair unverifiable,
 *     success:false, degraded services recorded;
 *  2. signal death (exit code null) -> same;
 *  3. non-zero exit WITH parseable probe diagnostics stays the legitimate
 *     incompatible path (tsc exits 1 on ordinary type errors — the guard
 *     must NOT be "fail on non-zero").
 */
export {};
