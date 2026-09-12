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

/**
 * What one repository on disk says it is on GitHub, and why it said nothing.
 *
 * `problem` is a clause, so a caller can put it after the path it read: it is
 * printed for every repo that contributes no identity, because a repo silently
 * left out of the project and connection steps looked like a complete run
 * (carrick#991).
 */
export type RepoIdentity = {
  path: string;
  /** `owner/repo`, or null when this repo names no GitHub repository. */
  name: string | null;
  /** The origin remote as written, for the line that says what was read. */
  remote: string | null;
  problem: string | null;
};

/** The two machine reads, injected so tests never spawn anything. */
export type IdentityProbe = {
  /** `git remote get-url origin`, or null when there is no origin. */
  remote: (repo: string) => string | null;
  /** The `hostname` ssh resolves a host to, or null when ssh cannot say. */
  sshHostname: (host: string) => string | null;
};

function gitOrigin(repo: string): string | null {
  const result = spawnSync("git", ["-C", repo, "remote", "get-url", "origin"], { encoding: "utf8", timeout: 5000 });
  return result.status === 0 ? result.stdout.trim() : null;
}

/**
 * The host ssh itself would dial, which for a per-account alias
 * (`Host github.com-work` / `HostName github.com`) is not the host in the
 * remote. `ssh -G` prints the whole resolved configuration and connects to
 * nothing, so this reads the user's own `~/.ssh/config` without a list of
 * alias spellings. An alias no config entry matches is not an error: ssh
 * exits 0 and echoes the name back, which is why an unresolvable alias is
 * detected by the hostname it returns rather than by a status.
 */
function sshHostname(host: string): string | null {
  // Never spawn on a string that could be read as an option, and never on one
  // ssh would reject anyway.
  if (!/^[A-Za-z0-9._-]+$/.test(host) || host.startsWith("-")) return null;
  const result = spawnSync("ssh", ["-G", host], {
    encoding: "utf8",
    timeout: 5000,
    stdio: ["ignore", "pipe", "ignore"],
  });
  if (result.error || result.status !== 0 || typeof result.stdout !== "string") return null;
  const line = /^hostname (\S+)$/im.exec(result.stdout);
  return line ? line[1]! : null;
}

const DEFAULT_PROBE: IdentityProbe = { remote: gitOrigin, sshHostname };

/** The host and `owner/repo` a remote names, in either spelling git accepts. */
function splitRemote(remote: string): { host: string; name: string | null; ssh: boolean } | null {
  let host: string;
  let repoPath: string;
  let ssh: boolean;
  // The two spellings are told apart by the `//`, not by whether `new URL`
  // throws: a host alias with no user (`host-work:owner/repo`) parses as a URL
  // whose scheme is the host, and would otherwise be read as neither form.
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(remote)) {
    let url: URL;
    try { url = new URL(remote); } catch { return null; }
    if (!["https:", "http:", "ssh:"].includes(url.protocol)) return null;
    host = url.hostname;
    repoPath = url.pathname;
    ssh = url.protocol === "ssh:";
  } else {
    // The scp spelling `[user@]host:owner/repo`, which is not a URL.
    const scp = /^(?:[^@\s/]+@)?([^:/\s]+):(\S+)$/.exec(remote);
    if (!scp) return null;
    host = scp[1]!;
    repoPath = scp[2]!;
    ssh = true;
  }
  const name = repoPath.replace(/^\/+/, "").replace(/\.git$/, "");
  return { host, name: /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(name) ? name : null, ssh };
}

/**
 * Only GitHub origin identities are sent to the workspace metadata read.
 *
 * A host that is already github.com is taken without spawning anything; an ssh
 * host that is not is resolved through the user's ssh configuration, because a
 * machine with two GitHub accounts writes its remotes through a per-account
 * alias and that repository is an ordinary GitHub repository (carrick#991,
 * carrick#978).
 */
export function repoIdentity(repo: string, probe: IdentityProbe = DEFAULT_PROBE): RepoIdentity {
  const remote = probe.remote(repo);
  if (remote === null) return { path: repo, name: null, remote: null, problem: "it has no origin remote" };
  const parts = splitRemote(remote);
  if (!parts) return { path: repo, name: null, remote, problem: `its origin ${remote} is not a GitHub URL` };
  const { host, name, ssh } = parts;
  if (name === null) {
    return { path: repo, name: null, remote, problem: `its origin ${remote} names no owner/repo path` };
  }
  if (host.toLowerCase() === "github.com") return { path: repo, name, remote, problem: null };
  if (!ssh) {
    return { path: repo, name: null, remote, problem: `its origin ${remote} is on ${host}, not github.com` };
  }
  const resolved = probe.sshHostname(host);
  if (resolved === null) {
    return { path: repo, name: null, remote, problem: `its origin ${remote} names the host "${host}", which ssh could not resolve` };
  }
  if (resolved.toLowerCase() === "github.com") return { path: repo, name, remote, problem: null };
  return {
    path: repo,
    name: null,
    remote,
    problem: `its origin ${remote} names the host "${host}", which ssh resolves to "${resolved}", not github.com`,
  };
}
