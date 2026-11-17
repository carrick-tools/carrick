import { reply } from "@fixture/reply";
export async function getJson<T>(path: string): Promise<T> {
  const res = await reply.get<T>(path);
  return res.data;
}
