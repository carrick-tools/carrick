import { reply } from "@fixture/reply";
export async function readName(path: string): Promise<string | null> {
  const res = await reply.get<string>(path);
  return res.data || null;
}
