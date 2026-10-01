const ACCOUNTS_API = process.env.ACCOUNTS_URL;

export async function listAccounts() {
  return fetch(`${ACCOUNTS_API}/accounts`, { method: "GET" });
}
