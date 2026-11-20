import { db } from "./client";

export async function writeAndReturn() {
  return await db.from("notes").insert({ body: "x" }).select();
}
