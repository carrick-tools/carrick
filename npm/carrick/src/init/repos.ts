import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { z } from "zod";
import { nativeEnv, resolveNativeBinary } from "../native.ts";

const service = z.object({ serviceName: z.string().nullable(), directory: z.string().optional(), tsconfig: z.string().optional() }).passthrough();
const proposal = z.object({
  schema: z.literal("carrick.derive/0"), workspace: z.string(), repos_detected_by: z.string(),
  repos_added: z.array(z.string()), repos_excluded: z.array(z.string()), missing: z.array(z.string()),
  parent_proposal: z.object({ directory: z.string(), repos: z.array(z.string()) }).nullable(),
  repos: z.array(z.object({ path: z.string(), reason: z.string(), services: z.array(service), config: z.record(z.unknown()).nullable(), warnings: z.array(z.string()) })),
});
export type WorkspaceProposal = z.infer<typeof proposal>;

/** Rust owns service selection, tsconfig precedence and configuration validation. */
export function deriveWorkspace(workspace: string): WorkspaceProposal {
  const native = resolveNativeBinary();
  if (!native.binary) throw new Error(native.problem ?? "The Carrick scanner is not installed.");
  const result = spawnSync(native.binary, ["derive", "--workspace", workspace, "--json"], {
    encoding: "utf8", env: nativeEnv(), timeout: 30_000, maxBuffer: 8 * 1024 * 1024,
  });
  if (result.error) throw new Error(`Could not derive the workspace: ${result.error.message}`);
  // The scanner prefixes its own command name, and every caller of this
  // prefixes theirs: without the strip, a discovery failure reads
  // "carrick init: carrick derive: no repos in ...", which looks like two
  // failures and hides the sentence that matters.
  if (result.status !== 0) throw new Error(result.stderr.trim().replace(/^carrick derive: /, "") || "The scanner could not derive the workspace.");
  const parsed = proposal.safeParse(JSON.parse(result.stdout));
  if (!parsed.success) throw new Error("The scanner returned an unsupported workspace proposal. Install matching Carrick CLI and scanner versions.");
  return parsed.data;
}

/** An explicit init may create a missing file once, including under a race. */
export function writeConfigs(plan: WorkspaceProposal): Array<{ path: string; created: boolean }> {
  return plan.repos.map((repo) => {
    const target = path.join(repo.path, "carrick.json");
    if (repo.config === null) return { path: target, created: false };
    try {
      fs.writeFileSync(target, `${JSON.stringify(repo.config, null, 2)}\n`, { flag: "wx" });
      return { path: target, created: true };
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "EEXIST") return { path: target, created: false };
      throw error;
    }
  });
}

/** Only GitHub origin identities are sent to the workspace metadata read. */
export function githubRemote(repo: string): string | null {
  const result = spawnSync("git", ["-C", repo, "remote", "get-url", "origin"], { encoding: "utf8", timeout: 5000 });
  if (result.status !== 0) return null;
  const remote = result.stdout.trim();
  const scp = /^git@github\.com:([A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+?)(?:\.git)?$/.exec(remote);
  if (scp) return scp[1]!;
  try {
    const url = new URL(remote);
    if (url.hostname.toLowerCase() !== "github.com" || !["https:", "ssh:"].includes(url.protocol)) return null;
    const name = url.pathname.replace(/^\//, "").replace(/\.git$/, "");
    return /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(name) ? name : null;
  } catch { return null; }
}
