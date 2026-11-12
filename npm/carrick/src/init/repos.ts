import { realPath } from "../fs-util.ts";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { z } from "zod";
import { splitRemote } from "../git-remote.ts";
import { nativeEnv, resolveNativeBinary } from "../native.ts";
import { versionFile } from "./outdated.ts";
import { excludedRepos } from "./workspace-file.ts";

const service = z.object({ serviceName: z.string().nullable(), directory: z.string().optional(), tsconfig: z.string().optional() }).passthrough();
const proposal = z.object({
  schema: z.literal("carrick.derive/0"), workspace: z.string(), repos_detected_by: z.string(),
  repos_added: z.array(z.string()), repos_excluded: z.array(z.string()), missing: z.array(z.string()),
  parent_proposal: z.object({ directory: z.string(), repos: z.array(z.string()) }).nullable(),
  repos: z.array(z.object({
    path: z.string(), reason: z.string(), services: z.array(service), config: z.record(z.unknown()).nullable(), warnings: z.array(z.string()),
    // The services whose dependencies a scan would refuse over, by the
    // scanner's own preflight rule (carrick#1489). Required: the scanner ships
    // in this package, so a document without it is a mismatched install and
    // is refused as one below.
    not_installed: z.array(z.object({ service: z.string(), directory: z.string(), command: z.string() })),
  })),
});
/**
 * One `carrick derive` run, parsed: the repos of this folder, the folder above
 * a single repository, and what a scan of each would refuse over. Nothing of
 * it is written anywhere: the config every scan runs under is Carrick's, held
 * for the project and derived in the cloud (carrick#3361).
 */
export type WorkspaceProposal = z.infer<typeof proposal>;

/** Rust owns repo discovery, service selection and configuration validation. */
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
  if (!parsed.success) throw new Error("The scanner described this workspace in a shape this CLI cannot read. Install matching Carrick CLI and scanner versions.");
  return parsed.data;
}

/** The same derivation with only these repos in it, by path. */
export function keepRepos(derived: WorkspaceProposal, keep: string[]): WorkspaceProposal {
  const kept = new Set(keep);
  return { ...derived, repos: derived.repos.filter((repo) => kept.has(repo.path)) };
}

/**
 * The folder above this repo, where an earlier `carrick init` there set this
 * repo up with the others; null otherwise.
 *
 * Run inside one repo of a folder that is already set up, init used to offer
 * this repo on its own, which is a second setup beside the first, so a repo
 * that setup covers is set up from the folder again (carrick#1512,
 * orchestrator ruling). The folder's own record of that run is the version
 * file init writes into its `.carrick/` (`versionFile`); the repos it covers
 * are the git repositories in it, holding a JavaScript or TypeScript
 * manifest, that its workspace file does not leave out. A folder that is
 * itself a repository is not a folder of repos (carrick#3361: this used to
 * read a proposal file that init no longer writes).
 */
export function initFolderAbove(repo: string): string | null {
  const here = realPath(repo);
  const folder = path.dirname(here);
  if (folder === here) return null;
  if (!fs.existsSync(versionFile(folder)) || fs.existsSync(path.join(folder, ".git"))) return null;
  if (!fs.existsSync(path.join(here, ".git")) || !JS_MANIFESTS.some((manifest) => fs.existsSync(path.join(here, manifest)))) return null;
  const excluded = new Set(excludedRepos(folder).map((name) => name.toLowerCase()));
  return excluded.has(path.basename(here).toLowerCase()) ? null : folder;
}

/**
 * Byte-identical to `write_self_ignore` in `src/local_mode/workspace.rs`.
 *
 * Two writers, because either command can be the first to create the
 * directory: `carrick index` writes it on every build, and `carrick init`
 * records its version there before any index exists. Same bytes so neither
 * rewrites the other's file. Keep the two in step.
 */
const SELF_IGNORE =
  "# Written by Carrick. Everything here is derived from your source and the\n" +
  "# hosted index, and `carrick refresh` rebuilds it.\n*\n";

/**
 * Create the workspace's `.carrick/`, ignored by git through its own
 * `.gitignore`, so nothing init or a later read keeps there is ever offered
 * to a commit.
 */
export function ensureIndexDir(workspace: string): void {
  const directory = path.join(workspace, ".carrick");
  fs.mkdirSync(directory, { recursive: true });
  fs.writeFileSync(path.join(directory, ".gitignore"), SELF_IGNORE);
}

/**
 * The repositories under a workspace, as this package counts them.
 *
 * A workspace is one repository or a folder of them, the same two shapes
 * `init` takes, so the scan is this directory and its immediate children that
 * hold a `.git` — a file counts as well as a directory, because a linked
 * worktree's `.git` is a file (carrick#975). Nothing is opened and nothing is
 * changed; `carrick remove` and `carrick doctor` both walk this list, and one
 * definition is what keeps the command that audits an install and the command
 * that undoes it looking at the same repos.
 */
export function repoRoots(workspace: string): string[] {
  const roots: string[] = [workspace];
  let children: fs.Dirent[] = [];
  try {
    children = fs.readdirSync(workspace, { withFileTypes: true });
  } catch {
    children = [];
  }
  for (const child of children) {
    if (!child.isDirectory() || child.name.startsWith(".")) continue;
    const candidate = path.join(workspace, child.name);
    if (fs.existsSync(path.join(candidate, ".git"))) roots.push(candidate);
  }
  return roots;
}

/**
 * The files that make a folder a JavaScript or TypeScript project, which is
 * what Carrick indexes. `carrick.json` is here because a folder holding one has
 * already been set up for Carrick.
 */
const JS_MANIFESTS = ["package.json", "deno.json", "deno.jsonc", "pnpm-workspace.yaml", "carrick.json"];

/**
 * The other repos in the folder above this one, where there are any.
 *
 * The scanner states the folder above a single repository and what it holds
 * (`parent_proposal`), by the same markers every scan uses, which count a
 * folder holding only `.git`, and one holding only a manifest. The question
 * these feed is which repos belong to the same system as this one, so a
 * sibling is kept only where it is a git repository (a `.git`, which is a file
 * in a linked worktree) holding a JavaScript or TypeScript manifest, and not
 * where the folder's own `carrick-workspace.json` already leaves it out
 * (carrick#1512).
 */
export function siblingRepos(
  plan: WorkspaceProposal,
  excluded: string[] = [],
  exists: (target: string) => boolean = fs.existsSync,
): string[] {
  const parent = plan.parent_proposal;
  if (parent === null) return [];
  const here = realPath(plan.workspace);
  return parent.repos.filter(
    (repo) =>
      path.dirname(repo) === parent.directory &&
      realPath(repo) !== here &&
      !excluded.includes(path.basename(repo)) &&
      exists(path.join(repo, ".git")) &&
      JS_MANIFESTS.some((manifest) => exists(path.join(repo, manifest))),
  );
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

/**
 * The repos `--repo` names, out of the repos this folder holds.
 *
 * `--repo` is the answer to "which of these does this install cover" without a
 * terminal, and it is also the only way to name a repository whose remote
 * could not (carrick#991): a value that matches nothing on disk names the one
 * repo here that has no GitHub identity, because that is the repo the flag was
 * introduced for and there is no other reading of it. Two unmatched values, or
 * one with no unnamed repo to attach it to, is a typo and is refused rather
 * than silently covering a different repo.
 */
export function selectRepos(
  candidates: RepoIdentity[],
  requested: string[],
): { repos: RepoIdentity[]; taken: RepoIdentity | null } | { problem: string } {
  const wanted = [...new Set(requested.map((name) => name.toLowerCase()))];
  const matched = new Map<string, RepoIdentity>();
  const unmatched: string[] = [];
  for (const name of wanted) {
    const found = candidates.find((repo) => repo.name !== null && repo.name.toLowerCase() === name);
    if (found) matched.set(found.path, found);
    else unmatched.push(name);
  }
  const unnamed = candidates.filter((repo) => repo.name === null);
  let taken: RepoIdentity | null = null;
  if (unmatched.length > 0) {
    const here = candidates
      .map((repo) => repo.name ?? path.basename(repo.path))
      .join(", ");
    if (unmatched.length > 1) {
      return { problem: `--repo ${unmatched.join(" and --repo ")} name no repo in this folder. The repos here are: ${here}.` };
    }
    if (unnamed.length !== 1) {
      return {
        problem:
          unnamed.length === 0
            ? `--repo ${unmatched[0]} names no repo in this folder. The repos here are: ${here}.`
            : `--repo ${unmatched[0]} names no repo in this folder, and ${unnamed.length} repos here have no GitHub identity, so it cannot name one of those either: ${unnamed.map((repo) => repo.path).join(", ")}.`,
      };
    }
    const original = requested.find((name) => name.toLowerCase() === unmatched[0])!;
    taken = { ...unnamed[0]!, name: original, problem: null };
    matched.set(taken.path, taken);
  }
  // Workspace order, whatever order the flags came in: everything downstream
  // reads this list, and the scanner's order is the one the derivation is in.
  return { repos: candidates.map((repo) => matched.get(repo.path)).filter((repo): repo is RepoIdentity => repo !== undefined), taken };
}

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
