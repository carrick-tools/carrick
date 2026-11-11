// The parts of a name and the pieces the request summaries read at each
// (carrick#3504 B10-2a1). Each probe is a call of a `p_*` function whose
// argument is the name; the test reads the pieces kept at each part of it.
import { API_BASE } from "./base";

declare const process: { env: Record<string, string | undefined> };

declare function p_url(path: string): void;
declare function p_env(path: string): void;
declare function p_built(path: string): void;
declare function p_base(path: string): void;
declare function p_local(path: string): void;
declare function p_items(path: string): void;

export function user(id: string): void {
  p_url(`/users/${id}`);
}

export function env(): void {
  p_env(`${process.env.API_URL ?? "http://localhost"}/items`);
}

export function built(q: string): void {
  p_built(`/a/${encodeURIComponent(q)}`);
}

export function based(): void {
  p_base(`${API_BASE}/things`);
}

export function local(id: string): void {
  const url = `/orders/${id}`;
  p_local(url);
}

export const itemsApi: { label?: string; replace(id: string): void } = {
  replace(id: string): void {
    p_items(`/items/${id}`);
  },
};

// A write through the object: the module does not read its members as
// functions of its own (carrick#3356), so `replace` is a function the
// module-level code writes, keyed by its definition (carrick#2793).
itemsApi.label = "items";

user("42");
itemsApi.replace("7");

// Read through callers as pieces (carrick#3504 B10-2a2).
declare const req: { params: { id: string } };
declare function p_wide(name: string): void;
declare function p_typed(kind: string): void;
declare function p_target(path: string): void;
declare function p_renamed(path: string): void;

// A second caller of `user`, passing a value read at runtime.
user(req.params.id);

// A parameter the compiler types as one literal.
export function typed(kind: "jobs.run"): void {
  p_typed(kind);
}

typed("jobs.run");

// A local of an environment base and a parameter, with no text of its
// own: the summaries read the local by its name, not as its initialiser.
export function target(id: string): void {
  const t = process.env.API_URL + id;
  p_target(t);
}

// A local holding one key of a parameter: the summaries read it by the
// local's name.
export function renamed(opts: { url: string }): void {
  const u = opts.url;
  p_renamed(u);
}

// A wrapper with seventeen callers, each passing its own literal.
export function wide(name: string): void {
  p_wide(name);
}

wide("w01");
wide("w02");
wide("w03");
wide("w04");
wide("w05");
wide("w06");
wide("w07");
wide("w08");
wide("w09");
wide("w10");
wide("w11");
wide("w12");
wide("w13");
wide("w14");
wide("w15");
wide("w16");
wide("w17");
