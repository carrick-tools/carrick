import { reply } from "@fixture/reply";
export function getThen<T>(path: string): Promise<T> {
  return reply.get<T>(path).then((r) => r.data);
}
