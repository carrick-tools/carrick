import { transmit } from "fixture-pending";

export function send(path: string, body: unknown): Promise<void> {
  return transmit(path, body);
}

export function sendViaClient(path: string, body: unknown): Promise<void> {
  return transmit(path, body);
}
