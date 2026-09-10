import { z } from "zod";
import { API_BASE } from "./credentials.ts";

const service = z.object({ service: z.string(), hash: z.string().nullable(), updated_at: z.string().nullable(), scanner_version: z.string().nullable() });
const repository = z.discriminatedUnion("connected", [
  z.object({ full_name: z.string(), connected: z.literal(false) }),
  z.object({ full_name: z.string(), connected: z.literal(true), project_id: z.string(), project_slug: z.string(), services: z.array(service) }),
]);
const response = z.object({
  schema: z.literal("carrick.resolve-repos/0"),
  workspace: z.object({ slug: z.string().regex(/^[a-zA-Z0-9_-]+$/), billing_tier: z.enum(["free", "cross_repo", "paid"]), installed: z.boolean() }),
  allowance_sentence: z.string().nullable(),
  repos: z.array(repository),
  project_repos: z.array(z.object({ project_slug: z.string(), repos: z.array(z.string()) })),
});
export type ResolvedRepos = z.infer<typeof response>;

/** Metadata only. This client never uploads source or requests model work. */
export async function resolveRepos(token: string, repos: string[], request: typeof fetch = fetch, signal?: AbortSignal): Promise<ResolvedRepos> {
  let result: Response;
  try {
    result = await request(`${API_BASE}/types/check-or-upload`, {
      method: "POST", redirect: "error", signal: AbortSignal.any([AbortSignal.timeout(30_000), ...(signal ? [signal] : [])]),
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ action: "resolve-repos", repos }),
    });
  } catch { throw new Error("Could not reach Carrick to verify the workspace. Check the connection and retry."); }
  if (result.status === 401) throw new Error("Carrick rejected this credential. Run carrick login (unset CARRICK_TOKEN first if it overrides your login).");
  if (!result.ok) throw new Error(`Carrick workspace lookup failed (HTTP ${result.status}).`);
  let payload: unknown;
  try { payload = await result.json(); }
  catch { throw new Error("Carrick workspace lookup returned invalid JSON."); }
  const parsed = response.safeParse(payload);
  if (!parsed.success) throw new Error("Carrick workspace lookup returned an unsupported or incomplete schema. The server must support carrick.resolve-repos/0.");
  return parsed.data;
}
