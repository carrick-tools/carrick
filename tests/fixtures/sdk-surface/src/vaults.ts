// A group of plain functions, published under one name by a namespace
// re-export in the entry (`export * as vaults from "./vaults.js"`). Consumers
// write `vaults.list(...)`, so the name they call is the export itself, not a
// value declared anywhere.
export function list(): Promise<unknown> {
  return fetch("/v1/vaults");
}

export function retrieve(id: string): Promise<unknown> {
  return fetch(`/v1/vaults/${id}`);
}

// A hop inside the group: the function is declared one module further on.
export { seal } from "./vaults/seal.js";
