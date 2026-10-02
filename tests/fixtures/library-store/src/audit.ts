import { relay } from "fixture-private-bus";

export async function audit(action: string): Promise<void> {
  await relay.send("audit.recorded", { action });
}
