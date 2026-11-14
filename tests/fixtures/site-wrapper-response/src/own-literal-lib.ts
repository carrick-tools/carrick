import { http } from "@fixture/http";
export async function store(path: string, body: unknown) {
  await http.post<{ id: string }>(path, body);
  return true;
}
