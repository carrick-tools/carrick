import { shelves } from "@/lib/shelves";
import { formatShelfLabel } from "@/lib/shelves";

export async function renameShelf(shelfId: string, label: string) {
  await shelves.rename(shelfId, formatShelfLabel(label));
}

export async function archiveShelf(shelfId: string) {
  await shelves.archive(shelfId);
}
