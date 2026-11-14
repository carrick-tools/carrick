import { bus } from "@fixture/bus";

async function announce(area: string, id: string): Promise<void> {
  await bus.publish(`${area}/created`, { headers: { id } });
}
export const announced = (id: string) => announce("orders", id);
