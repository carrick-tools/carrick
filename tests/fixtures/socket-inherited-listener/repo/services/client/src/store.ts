import Client from "@fixture/live-store";

export function listen(client: Client): void {
  client.on("s:message", () => undefined);
}

export function drain(client: Client): void {
  const cursor = client.cursor();
  cursor.on("data", () => undefined);
}

export class Bus extends Client {}

export function relay(bus: Bus): void {
  bus.on("s:bus", () => undefined);
}

export const shared = new Client();
