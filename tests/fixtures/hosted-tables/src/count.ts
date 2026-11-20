import { db } from "./client";

export async function countAccounts() {
  return await db.from("accounts").select("*", { head: true });
}
