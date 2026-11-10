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
