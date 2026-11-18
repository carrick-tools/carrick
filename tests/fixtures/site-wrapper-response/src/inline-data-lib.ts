import { reply } from "@fixture/reply";
export async function getQ(u: string) {
  return (await reply.get<{ q: number }>(u)).data;
}
