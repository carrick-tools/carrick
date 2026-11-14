import { http } from "@fixture/http";
export async function wipe(path: string) {
  await http.purge<{ purged: number }>(path);
}
