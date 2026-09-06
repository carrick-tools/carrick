// Logging that can never reach a protocol stream.
//
// The language server owns stdout for JSON-RPC and the hooks own stdout for the
// context they print, so every line here goes to stderr, and to a file when
// CARRICK_LOG names one. Failing to log is not an error worth raising.

import fs from "node:fs";
import path from "node:path";

export type Logger = (...parts: unknown[]) => void;

export function createLogger(prefix: string, env: NodeJS.ProcessEnv = process.env): Logger {
  const file = env["CARRICK_LOG"] ?? null;
  const quiet = env["CARRICK_LOG_QUIET"] === "1";
  return (...parts: unknown[]): void => {
    const text = parts
      .map((part) => (typeof part === "string" ? part : JSON.stringify(part)))
      .join(" ");
    const line = `[${new Date().toISOString()}] ${prefix}: ${text}\n`;
    if (file) {
      try {
        fs.mkdirSync(path.dirname(path.resolve(file)), { recursive: true });
        fs.appendFileSync(file, line);
      } catch {
        // A log that cannot be written is not a reason to fail an edit.
      }
    }
    if (quiet) return;
    try {
      process.stderr.write(line);
    } catch {
      // Same.
    }
  };
}
