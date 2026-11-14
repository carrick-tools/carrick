import { sendBoth, sendStopOnly, retryBoth, sendAndRun, runOnly, sendAndHook, hookOnly, Port } from "@fixture/client";

export async function relayBoth(port: Port): Promise<void> {
  return sendBoth(port);
}

export async function relayStopOnly(port: Port): Promise<void> {
  return sendStopOnly(port);
}

const ports: Record<string, Port> = {};

export async function relayKeyed(name: string): Promise<void> {
  return sendBoth(ports[name]);
}

export async function relayKeyedStopOnly(name: string): Promise<void> {
  return sendStopOnly(ports[name]);
}

export async function relayCallback(cb: (name: string, body: unknown) => Promise<void>): Promise<void> {
  return sendBoth({ send: cb });
}

export async function relayCallbackStopOnly(cb: (name: string, body: unknown) => Promise<void>): Promise<void> {
  return sendStopOnly({ send: cb });
}

export class Holder {
  constructor(private readonly port: Port) {}
  relayField(): Promise<void> {
    return sendBoth(this.port);
  }
  relayFieldStopOnly(): Promise<void> {
    return sendStopOnly(this.port);
  }
}

export async function relayRetry(): Promise<void> {
  return retryBoth(3);
}

export async function relayRun(run: () => Promise<void>): Promise<void> {
  return sendAndRun(run);
}

export async function relayRunOnly(run: () => Promise<void>): Promise<void> {
  return runOnly(run);
}

export async function relayRunWritten(): Promise<void> {
  return sendAndRun(async () => undefined);
}

export async function relayHook(): Promise<void> {
  return sendAndHook();
}

export async function relayHookOnly(): Promise<void> {
  return hookOnly();
}

let unsetPort: Port;

export async function relayUnset(): Promise<void> {
  return sendBoth(unsetPort);
}

export async function relayUnsetStopOnly(): Promise<void> {
  return sendStopOnly(unsetPort);
}
