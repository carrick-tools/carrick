// Requests after an exit, or inside a branch, under a test of a parameter
// the caller decides (carrick#4093). Answer key: README.md, "Tests on the
// path to a request".

// g1: the URL is a parameter; a method test exits before the send.
export class ExitBeforeSend {
  base = "/api";
  send = async (path: string, verb: string): Promise<unknown> => {
    if (verb === "GET") return null;
    const res = await fetch(this.base + path, { method: "POST" });
    return res.json();
  };
}

// g2: each request goes through a helper, in a branch the caller picks.
export class BranchToHelper {
  base = "/api";
  post = async (path: string): Promise<unknown> => {
    const res = await fetch(this.base + path, { method: "POST" });
    return res.json();
  };
  sync = async (kind: string): Promise<unknown> => {
    if (kind === "PUSH") {
      return this.post("/sync/push");
    }
    return this.post("/sync/pull");
  };
}

// g3: a prefix test of an options key throws before the send.
export class PrefixExit {
  send = async (path: string, opts: { mode: string }): Promise<unknown> => {
    if (opts.mode.startsWith("dry")) {
      throw new Error("dry run");
    }
    const res = await fetch(`/api${path}`, { method: "POST" });
    return res.json();
  };
}

// g4: a literal URL after an exit: the send line states it whatever the
// callers pass.
export async function literalAfterExit(verb: string): Promise<unknown> {
  if (verb !== "PUT") return null;
  const res = await fetch("/api/literal", { method: "PUT" });
  return res.json();
}
