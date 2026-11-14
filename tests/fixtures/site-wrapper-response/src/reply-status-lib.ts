import { reply } from "@fixture/reply";
export async function getStatus(path: string) {
  const res = await reply.get<{ name: string }>(path);
  return res.status;
}
