import { bus } from "@fixture/bus";

export async function shout(area: string, id: string): Promise<void> {
  await bus.publish(`/${area}/shipped`, { headers: { id } });
}
