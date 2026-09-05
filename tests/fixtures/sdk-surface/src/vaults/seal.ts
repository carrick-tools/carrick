export function seal(id: string): Promise<unknown> {
  return fetch(`/v1/vaults/${id}/seal`, { method: "POST" });
}
