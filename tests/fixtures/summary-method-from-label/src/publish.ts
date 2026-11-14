import { bus } from "@fixture/bus";

async function notify(area: string, id: string): Promise<void> {
  await bus.publish(`/${area}/created`, { headers: { id } });
}
export const notified = (id: string) => notify("orders", id);
