import { db } from "./client";

export async function uploadPicture(body: Blob) {
  return await db.storage.from("pics").upload("a.png", body);
}
