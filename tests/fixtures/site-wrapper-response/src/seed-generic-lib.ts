import { reply } from "@fixture/reply";
export interface Order { id: string }
export async function callApi<T>(path: string): Promise<T> {
  const res = await reply.get<T>(path);
  return res.data;
}
