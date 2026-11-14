import { reply } from "@fixture/reply";
export function getReply(path: string) {
  return reply.get<{ city: string }>(path);
}
