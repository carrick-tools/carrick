import { reply } from "@fixture/reply";
export function items(p: string) {
  return reply.get<{ items: string[] }>(p).then((r) => r.data.items);
}
