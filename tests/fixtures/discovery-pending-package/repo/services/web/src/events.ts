import { send } from "./client";

export function postEvent(body: unknown): Promise<void> {
  return send("/events", body);
}
