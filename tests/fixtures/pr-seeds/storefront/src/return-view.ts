import { queryClient } from "./query-kit.js";
import { getReturnOptions } from "./gen/returns.options.js";

export async function itemCountOf(returnId: string): Promise<number> {
  const found = await queryClient.fetchQuery(getReturnOptions(returnId));
  return found.items;
}
