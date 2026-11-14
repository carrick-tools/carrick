import { reply } from "@fixture/reply";
export async function getName(path: string) {
  const res = await reply.get<{ name: string }>(path);
  return res.data;
}
