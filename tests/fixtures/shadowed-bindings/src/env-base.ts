const API = process.env.API_URL;

export async function envBase(internal: boolean) {
  if (internal) {
    const API = "/internal-prefix";
    return fetch(`${API}/users`, { method: "POST" });
  }
  return null;
}
