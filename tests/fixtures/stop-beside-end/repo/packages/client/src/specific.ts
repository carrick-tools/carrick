import { Port } from "./mixed";

export async function sendThenRun(port: Port, run: () => Promise<void>): Promise<void> {
  await fetch("/v1/api/send-then-run");
  await port.send("s", {});
  await run();
}

export async function thenRunOnly(port: Port, run: () => Promise<void>): Promise<void> {
  await port.send("s", {});
  await run();
}
