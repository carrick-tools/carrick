import { reply } from "@fixture/reply";
export async function readNick(path: string): Promise<string | null> {
  try {
    const res = await reply.get<string>(path);
    return res.data;
  } catch {
    return null;
  }
}
