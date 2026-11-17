import { http } from "@fixture/http";
interface Total { sum: number }
export async function sum(path: string) {
  return (await http.post(path)) as Total;
}
