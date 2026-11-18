import { http } from "@fixture/http";
declare const db: { users: { find(id: string): { row: number } } };
export async function save(p: string, id: string) {
  await http.post<{ saved: boolean }>(p);
  return db.users.find(id);
}
