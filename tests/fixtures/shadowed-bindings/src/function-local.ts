export async function localShadow(admin: boolean) {
  const ITEMS = "/api/items";
  if (admin) {
    const ITEMS = "/api/admin-items";
    return fetch(ITEMS, { method: "POST" });
  }
  return fetch(ITEMS, { method: "GET" });
}
