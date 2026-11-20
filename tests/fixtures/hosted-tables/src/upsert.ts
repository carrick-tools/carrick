import { db } from "./client";

export async function saveAccount() {
  return await db.from("accounts").upsert({ id: 1 });
}
