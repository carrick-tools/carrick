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
    // Read per line, so `withQuietLog` can silence loggers made at import time.
    if (env["CARRICK_LOG_QUIET"] === "1") return;
    try {
      process.stderr.write(line);
    } catch {
      // Same.
    }
  };
}

/**
 * Run `work` with every logger's terminal line off (a log file named by
 * CARRICK_LOG still gets its lines). `carrick init` prints only its progress
 * line and step results, so a hook-style log line must not land in the middle.
 */
export async function withQuietLog<T>(work: () => Promise<T>): Promise<T> {
  const before = process.env["CARRICK_LOG_QUIET"];
  process.env["CARRICK_LOG_QUIET"] = "1";
  try {
    return await work();
  } finally {
    if (before === undefined) delete process.env["CARRICK_LOG_QUIET"];
    else process.env["CARRICK_LOG_QUIET"] = before;
  }
}
