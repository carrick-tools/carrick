import { http } from "@fixture/http";

function makeTrimmer(root: string | undefined) {
  return () => http.remove<{ trimmed: number }>(`${root}/trim`, { headers: {} });
}
export const trim = makeTrimmer(process.env.API_BASE);
