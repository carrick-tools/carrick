// The one client of the compiler engine's stdio protocol: write the requests
// (one JSON line each), close, and read each request's last non-progress frame.
// A process per call, so the engine's memory is returned when it answers.

import { spawn } from "node:child_process";

export type EngineRequest = { action: string; request_id: string } & Record<string, unknown>;
export type EngineFrame = { request_id?: string; status?: string; errors?: string[] } & Record<string, unknown>;

export type AskOptions = {
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  /** The process is killed after this long. */
  timeoutMs: number;
};

/**
 * Run the engine at `entry` over `requests` (a `shutdown` follows them) and
 * answer each request's final frame by `request_id`. Rejects when the process
 * cannot start or answers none of the requests.
 */
export function askEngine(entry: string, requests: EngineRequest[], options: AskOptions): Promise<Map<string, EngineFrame>> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [entry], {
      ...(options.cwd === undefined ? {} : { cwd: options.cwd }),
      env: options.env ?? process.env,
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => child.kill("SIGKILL"), options.timeoutMs);
    child.stdout.on("data", (chunk: Buffer) => (stdout += chunk.toString("utf8")));
    child.stderr.on("data", (chunk: Buffer) => (stderr = (stderr + chunk.toString("utf8")).slice(-2000)));
    child.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.on("close", () => {
      clearTimeout(timer);
      const frames = new Map<string, EngineFrame>();
      for (const line of stdout.split("\n")) {
        if (!line.trim()) continue;
        try {
          const frame = JSON.parse(line) as EngineFrame;
          if (typeof frame.request_id === "string" && frame.status !== "progress") frames.set(frame.request_id, frame);
        } catch {
          // Not a protocol line.
        }
      }
      if (frames.size === 0) reject(new Error(`no answer from the compiler engine: ${stderr}`));
      else resolve(frames);
    });
    child.stdin.write([...requests, { action: "shutdown", request_id: "shutdown" }].map((r) => JSON.stringify(r)).join("\n") + "\n");
    child.stdin.end();
  });
}
