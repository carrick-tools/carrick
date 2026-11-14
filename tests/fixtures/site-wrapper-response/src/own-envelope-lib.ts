import { http } from "@fixture/http";
export async function file(path: string) {
  const body = await http.post<{ ref: string }>(path);
  return { body, at: 1 };
}
