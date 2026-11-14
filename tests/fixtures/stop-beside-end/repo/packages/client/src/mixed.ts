export interface Port {
  send(name: string, body: unknown): Promise<void>;
}

export async function sendBoth(port: Port): Promise<void> {
  await fetch("/v1/api/both");
  await port.send("both", {});
}

export async function sendStopOnly(port: Port): Promise<void> {
  await port.send("only", {});
}

export async function retryBoth(attempts: number): Promise<void> {
  await fetch("/v1/api/retry");
  if (attempts > 0) await retryBoth(attempts - 1);
}

export async function sendAndRun(run: () => Promise<void>): Promise<void> {
  await fetch("/v1/api/run");
  await run();
}

export async function runOnly(run: () => Promise<void>): Promise<void> {
  await run();
}

let hook: () => Promise<void>;

export async function sendAndHook(): Promise<void> {
  await fetch("/v1/api/hook");
  await hook();
}

export async function hookOnly(): Promise<void> {
  await hook();
}
