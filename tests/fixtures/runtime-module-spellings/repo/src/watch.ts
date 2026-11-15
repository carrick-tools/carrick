import { spawn } from "node:child_process";

export function watch(command: string): void {
  const child = spawn(command);
  child.on("job-finished", (message: unknown) => {
    console.log(message);
  });
}
