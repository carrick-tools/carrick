import { db } from "./client";

export async function writeNote() {
  return await db.from("notes").insert({ body: "x" });
}
