export class Vault {
  restore(id: string): Promise<unknown> {
    return fetch(`/v1/archive/${id}/restore`, { method: "POST" });
  }
}
