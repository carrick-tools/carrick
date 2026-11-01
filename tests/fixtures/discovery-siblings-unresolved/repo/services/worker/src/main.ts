import { postTo } from "@fixture/client";

export async function heartbeat() {
  return postTo("/api/beats", { at: 1 });
}
