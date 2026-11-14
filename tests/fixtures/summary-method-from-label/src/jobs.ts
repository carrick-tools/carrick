import { http } from "@fixture/http";

async function job(base: string, method?: string): Promise<number> {
  const out = await http.request<{ n: number }>(`${base}/api/jobs`, { method, headers: {} });
  return out.n;
}
export const listJobs = () => job("/q");
export const postJob = () => job("/q", "POST");
