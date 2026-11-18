import { reply } from "@fixture/reply";
export async function first(p: string) {
  return (await reply.get<{ id: string }[]>(p)).data[0];
}
