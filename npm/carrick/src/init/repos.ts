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

/**
 * One `carrick derive` run: the document as the scanner printed it, and the
 * same document parsed.
 *
 * Both halves are kept because they are read by different consumers. `plan` is
 * what this CLI prints and decides on; `document` is what lands in
 * `.carrick/proposal.json` for the scaffold tool's agent, and it has to be the
 * scanner's own bytes — the schema above is not a passthrough at the top level,
 * so parsing and re-serialising would silently drop any field a later
 * `carrick.derive/0` adds.
 */
export type DerivedWorkspace = { plan: WorkspaceProposal; document: string };

/** Rust owns service selection, tsconfig precedence and configuration validation. */
export function deriveWorkspace(workspace: string): DerivedWorkspace {
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
  return { plan: parsed.data, document: result.stdout };
}

/** The seam document, relative to the workspace root (carrick-cloud#799). */
export const PROPOSAL_FILE = path.join(".carrick", "proposal.json");

/**
 * Byte-identical to `write_self_ignore` in `src/local_mode/workspace.rs`.
 *
 * Two writers, because either command can be the first to create the
 * directory: `carrick index` writes it on every build, and `carrick init` now
 * writes the proposal before any index exists. Same bytes so neither rewrites
 * the other's file. Keep the two in step.
 */
const SELF_IGNORE =
  "# Written by Carrick. Everything here is derived from your source and is\n" +
  "# rebuilt by re-running `carrick index`.\n*\n";

/**
 * Write the derived proposal where the scaffold tool's agent reads it.
 *
 * This is the whole of what init leaves in the workspace besides the hook
 * settings: the agent seeds `carrick.json` from this document, and nothing
 * this command derived without a model is committed to the repository
 * (carrick-cloud#799). Rewritten on every run rather than created once — it is
 * a derived file in an ignored directory, and a stale one would seed the
 * config from a workspace that has since changed.
 */
export function writeProposal(workspace: string, derived: DerivedWorkspace): string {
  const directory = path.join(workspace, ".carrick");
  fs.mkdirSync(directory, { recursive: true });
  fs.writeFileSync(path.join(directory, ".gitignore"), SELF_IGNORE);
  const document = derived.document.endsWith("\n") ? derived.document : `${derived.document}\n`;
  fs.writeFileSync(path.join(workspace, PROPOSAL_FILE), document);
  return PROPOSAL_FILE;
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
