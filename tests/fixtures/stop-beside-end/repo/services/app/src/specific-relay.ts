import { sendThenRun, thenRunOnly, Port } from "@fixture/client";

const ports: Record<string, Port> = {};

export async function relayThenRun(name: string, run: () => Promise<void>): Promise<void> {
  return sendThenRun(ports[name], run);
}

export async function relayThenRunOnly(name: string, run: () => Promise<void>): Promise<void> {
  return thenRunOnly(ports[name], run);
}
