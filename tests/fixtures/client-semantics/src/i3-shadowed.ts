import { api } from "./shared/api";

export function i3(): string | undefined {
  const api = new Map<string, string>();
  return api.get("/shadowed");
}

export function i3b(): unknown { return api.get("/unshadowed"); }
