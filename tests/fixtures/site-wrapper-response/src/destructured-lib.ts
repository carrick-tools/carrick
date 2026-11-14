import { reply } from "@fixture/reply";
export async function getZip(path: string) {
  const { data } = await reply.get<{ zip: string }>(path);
  return data;
}
