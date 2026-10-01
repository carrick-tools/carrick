const USERS = "/api/users";

export async function load(admin: boolean) {
  if (admin) {
    const USERS = "/api/admins";
    return fetch(USERS, { method: "POST" });
  }
  return null;
}

export async function loadAll() {
  return fetch(USERS, { method: "PUT" });
}
