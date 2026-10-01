export async function removeItem(path: string, admin: boolean) {
  if (admin) {
    const path = "/api/param-admins";
    return fetch(path, { method: "DELETE" });
  }
  return null;
}

export async function archiveItem(path: string) {
  return fetch(path, { method: "PATCH" });
}
