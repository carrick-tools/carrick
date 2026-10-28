import { sendViaClient } from "./client";

type SendFn = (path: string, body: unknown) => Promise<void>;

export function notify(body: unknown, deps: { send?: SendFn } = {}): Promise<void> {
  const doSend = deps.send ?? sendViaClient;
  return doSend("/notify", body);
}
