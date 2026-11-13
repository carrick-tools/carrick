// URL locals written more than once around an exit, a falsy initialiser, a
// loop, a closure, a read of the local itself and a nested block
// (carrick#3679). Answer key: README.md, "Exits and falsy fallbacks".

// e1: a branch writes the local and returns: the fetch below never sees it.
export class EarlyReturn {
  base = "/api";
  post = async (path: string, mock?: boolean): Promise<unknown> => {
    let target = this.base + path;
    if (mock) {
      target = "/mock" + path;
      return null;
    }
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}

// e2: the same, under a test the caller decides.
export class EarlyReturnGuarded {
  base = "/api";
  send = async (path: string, verb: string): Promise<unknown> => {
    let target = this.base + path;
    if (verb === "GET") {
      target = "/cache" + path;
      return null;
    }
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}

// e3: an empty initialiser, then `q || path`.
export class EmptyInit {
  base = "/api";
  send = async (path: string, verb: string): Promise<unknown> => {
    let withQuery = "";
    if (verb === "GET") {
      withQuery = `${path}?q=1`;
    }
    const target = this.base + (withQuery || path);
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}

// e3n: a null initialiser, then `q || path`.
export class NullInit {
  base = "/api";
  send = async (path: string, verb: string): Promise<unknown> => {
    let withQuery: string | null = null;
    if (verb === "GET") {
      withQuery = `${path}?q=1`;
    }
    const target = this.base + (withQuery || path);
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}

// e3q: an empty initialiser, then `q ?? path`: `""` is not nullish.
export class EmptyNullish {
  base = "/api";
  send = async (path: string, verb: string): Promise<unknown> => {
    let withQuery = "";
    if (verb === "GET") {
      withQuery = `${path}?q=1`;
    }
    const target = this.base + (withQuery ?? path);
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}

// e4: written after the request.
export class AfterUse {
  base = "/api";
  post = async (path: string): Promise<unknown> => {
    let target = this.base + path;
    const res = await fetch(target, { method: "POST" });
    target = "/other";
    return res.json();
  };
}

// e5: written in a loop.
export class InLoop {
  base = "/api";
  post = async (path: string, parts: string[]): Promise<unknown> => {
    let target = this.base + path;
    for (const part of parts) {
      target = this.base + part;
    }
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}

// e6: a closure reads it before a later write.
export class ClosureRead {
  base = "/api";
  post = async (path: string): Promise<unknown> => {
    let target = this.base + path;
    const go = () => fetch(target, { method: "POST" });
    target = "/late" + path;
    const res = await go();
    return res.json();
  };
}

// e7: a branch writes and throws.
export class ThrowBranch {
  base = "/api";
  post = async (path: string, strict?: boolean): Promise<unknown> => {
    let target = this.base + path;
    if (strict) {
      target = "/strict" + path;
      throw new Error(target);
    }
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
}

// e8: written only under a test the caller decides false: unwritten at use.
export class UnwrittenAtUse {
  base = "/api";
  send = async (path: string, verb: string): Promise<unknown> => {
    let target: string | undefined;
    if (verb === "GET") {
      target = this.base + path;
    }
    const res = await fetch(target as string, { method: "POST" });
    return res.json();
  };
}

// f8: the local extended from itself under a test the caller decides.
export class SelfExtend {
  base = "/api";
  send = async (path: string, verb: string): Promise<unknown> => {
    let target = this.base + path;
    if (verb === "GET") target = target + "?q=1";
    const res = await fetch(target, { method: verb });
    return res.json();
  };
  post = async (path: string): Promise<unknown> => this.send(path, "POST");
}

// a7b: the test and the read in one block below the declaration.
export class SameBlock {
  base = "/api";
  send = async (path: string, verb: string): Promise<unknown> => {
    let t = this.base + path;
    {
      if (verb === "GET") {
        t = "/cache" + path;
      }
      await fetch(t, { method: "POST" });
    }
    return null;
  };
}

// dup: a query set under a truthiness test: both ways key one route.
export class QueryWritten {
  get = async (q?: string): Promise<unknown> => {
    let url = "/api/c4";
    if (q) url = "/api/c4?query=1";
    const res = await fetch(url);
    return res.json();
  };
}
