import { vaults } from "../index.js";

// A SAME-repo call through a namespace re-export: the callee is a member of a
// name the entry publishes, not of anything this file imports directly.
export function refresh(): Promise<unknown> {
  return vaults.list();
}
