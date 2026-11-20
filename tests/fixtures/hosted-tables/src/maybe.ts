import { db } from "./client";

// The `head` key is not a literal: the call sends HEAD or GET at run time.
export async function countAccountsMaybe(head: boolean) {
  return await db.from("accounts").select("*", { head });
}
