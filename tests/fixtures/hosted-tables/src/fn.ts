import { db } from "./client";

export async function bump() {
  return await db.rpc("bump_count", {});
}
