import { http } from "@fixture/http";
export async function inner(p: string): Promise<{ status: number; body: { e: number } }> {
  const body = await http.post<any>(p);
  return { status: 200, body };
}
