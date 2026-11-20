import { db } from "./client";

export async function editNote() {
  return await db.from("notes").update({ body: "y" }).eq("id", 1);
}

export async function dropNote() {
  return await db.from("notes").delete().eq("id", 1);
}
