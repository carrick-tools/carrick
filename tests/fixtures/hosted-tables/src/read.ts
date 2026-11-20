import { db } from "./client";

export async function readAccount() {
  return await db.from("accounts").select("*").eq("id", 1);
}
