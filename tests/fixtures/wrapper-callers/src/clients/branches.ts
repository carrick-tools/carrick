// URL locals written more than once, one form per class. Each sends through
// `fetch`, so every row at a caller is read off the source.

// a1: a `let` with no initialiser, set in both branches of a prefix test.
export class BothBranches {
  base = "/api";
  post = async (path: string): Promise<unknown> => {
    let target: string;
    if (path.startsWith("http")) {
      target = path;
    } else {
      target = this.base + path;
    }
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}

// a2: a `let` with an initialiser, set again under a prefix test.
export class Reassigned {
  base = "/api";
  post = async (path: string): Promise<unknown> => {
    let target = this.base + path;
    if (path.startsWith("http")) {
      target = path;
    }
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}

// a3: a URL that reads a `let` set only when data is given, or the path.
export class OrQuery {
  base = "/api";
  post = async (path: string, data?: object): Promise<unknown> => {
    let withQuery: string | undefined;
    if (data) {
      withQuery = `${path}?q=1`;
    }
    const target = this.base + (withQuery || path);
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}

// a6: a `let` set inside a callback, which runs when the callee says.
export class CallbackWrite {
  base = "/api";
  post = async (path: string, hooks: string[]): Promise<unknown> => {
    let target = this.base + path;
    hooks.forEach((hook) => {
      target = hook;
    });
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}

// a7: the branching client. The query is added only for a GET, and an
// absolute URL is sent as it is.
export class BranchClient {
  base = "/api";
  send = async <T = unknown>(path: string, verb: "GET" | "POST", data?: object): Promise<T> => {
    let withQuery: string | undefined;
    let target: string;
    if (verb === "GET") {
      withQuery = data ? `${path}?q=1` : path;
    }
    if (path.match(/^http/)) {
      target = withQuery || path;
    } else {
      target = this.base + (withQuery || path);
    }
    const res = await fetch(target, { method: verb });
    return (await res.json()) as T;
  };
  post = <T = unknown>(path: string, data?: object): Promise<T> => this.send<T>(path, "POST", data);
}

// a8: one statement sets two locals, the URL from the query it also sets.
export class TwoLocals {
  base = "/api";
  post = async (path: string, data?: object): Promise<unknown> => {
    let query = "";
    let target = this.base + path;
    if (data) {
      query = "?q=1";
      target = this.base + path + query;
    }
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}
