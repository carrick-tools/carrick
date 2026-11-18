import { createClient } from "@acme/sdk";

export const client = createClient("https://example.invalid");

export async function welcome(user: string) {
  return client.functions.invoke("send-welcome", { body: { user } });
}

export async function health() {
  return client.ping();
}
