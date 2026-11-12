// Reading a git remote URL. No dependencies, so the hooks and the background
// fact refresh can load it without the package's installed modules.

/** The host and `owner/repo` a remote names, in either spelling git accepts. */
export function splitRemote(remote: string): { host: string; name: string | null; ssh: boolean } | null {
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
