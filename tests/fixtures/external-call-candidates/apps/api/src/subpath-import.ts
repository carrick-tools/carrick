import { publishEdge } from "courier-sdk/edge";

export function ping(id: string): Promise<void> {
  return publishEdge(id);
}
