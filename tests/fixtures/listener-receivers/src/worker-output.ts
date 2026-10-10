import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

// Reads a child process's output one line at a time.
export function watchWorker(): void {
  const child = spawn("worker", ["--serve"]);
  createInterface({ input: child.stdout }).on("line", (line: string) => {
    console.log(line);
  });
}
