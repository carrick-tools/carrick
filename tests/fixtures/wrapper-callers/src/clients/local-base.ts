// A URL written once into a local: an unstated base beside a path the caller
// passes. Each function below has an inline twin that writes the same URL in
// the call itself; the twin and the local must state the same request.
declare function getBase(): string;
type Options = { base?: string };

export class Gateway {
  base: string;
  constructor(o: Options = {}) {
    this.base = o.base || "/api";
  }

  // Local: the caller's option, else the field.
  sendLocal = async (path: string, opts: Options = {}): Promise<unknown> => {
    const target = (opts.base ?? this.base) + path;
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
  sendInline = async (path: string, opts: Options = {}): Promise<unknown> => {
    const res = await fetch((opts.base ?? this.base) + path, { method: "POST" });
    return res.json();
  };

  // Local: the field alone, set by the constructor.
  fieldLocal = async (path: string): Promise<unknown> => {
    const target = this.base + path;
    const res = await fetch(target, { method: "POST" });
    return res.json();
  };
  fieldInline = async (path: string): Promise<unknown> => {
    const res = await fetch(this.base + path, { method: "POST" });
    return res.json();
  };
}

export async function getterLocal(path: string): Promise<unknown> {
  const target = getBase() + path;
  const res = await fetch(target, { method: "POST" });
  return res.json();
}
export async function getterInline(path: string): Promise<unknown> {
  const res = await fetch(getBase() + path, { method: "POST" });
  return res.json();
}

export async function envLocal(path: string): Promise<unknown> {
  const target = process.env.LOCAL_BASE_URL + path;
  const res = await fetch(target, { method: "POST" });
  return res.json();
}
export async function envInline(path: string): Promise<unknown> {
  const res = await fetch(process.env.LOCAL_BASE_URL + path, { method: "POST" });
  return res.json();
}

export async function templateLocal(path: string, opts: Options = {}): Promise<unknown> {
  const target = `${opts.base ?? "/api"}${path}`;
  const res = await fetch(target, { method: "POST" });
  return res.json();
}
export async function templateInline(path: string, opts: Options = {}): Promise<unknown> {
  const res = await fetch(`${opts.base ?? "/api"}${path}`, { method: "POST" });
  return res.json();
}

// A local holding a literal piece keeps what it held before.
export async function literalLocal(path: string): Promise<unknown> {
  const target = "/v2" + path;
  const res = await fetch(target, { method: "POST" });
  return res.json();
}

// A local with no parameter beside its pieces stays written by its name.
export async function noParameterLocal(): Promise<unknown> {
  const target = getBase() + process.env.LOCAL_TAIL;
  const res = await fetch(target, { method: "POST" });
  return res.json();
}
