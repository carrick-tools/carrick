import { bus } from "@fixture/bus";

async function record(area: string, total: number): Promise<void> {
  await bus.publish(`/${area}/totals`, { data: { total } });
}
export const recorded = (total: number) => record("ledger", total);
