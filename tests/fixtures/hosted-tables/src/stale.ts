import { db } from "./client";

export async function readGhosts() {
  return await db.from("ghosts").select();
}
