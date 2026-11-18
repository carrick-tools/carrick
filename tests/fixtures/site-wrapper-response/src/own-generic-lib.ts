import { http } from "@fixture/http";
export async function probe<T>(path: string): Promise<boolean> {
  const res = await http.post<T>(path);
  return res !== undefined;
}
