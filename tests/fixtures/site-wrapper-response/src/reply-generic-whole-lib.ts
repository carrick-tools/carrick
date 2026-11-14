import { reply } from "@fixture/reply";
export function getAny<T>(path: string) {
  return reply.get<T>(path);
}
