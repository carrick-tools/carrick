import { spawn } from "child_process";

export function run(command: string): void {
  const proc = spawn(command);
  proc.on("job-started", (code: number) => {
    console.log(code);
  });
}
