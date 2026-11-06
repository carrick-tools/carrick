/**
 * Small filesystem and shape helpers the CLI repeated in several modules. Node
 * built-ins only, so a hook entry point can import it without a startup cost.
 */
import fs from "node:fs";
import path from "node:path";

/** Whether the path exists; any error reads as "no". The default for an injected `exists`. */
export function defaultExists(target: string): boolean {
  try {
    return fs.existsSync(target);
  } catch {
    return false;
  }
}

/** A path with its symlinks resolved, or as given (made absolute) where it cannot be. */
export function realPath(target: string): string {
  try {
    return fs.realpathSync(target);
  } catch {
    return path.resolve(target);
  }
}

/** A plain object: not null, not an array. */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
