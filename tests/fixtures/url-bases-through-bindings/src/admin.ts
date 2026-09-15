const ACCOUNTS_PATH = "/admin/accounts";

export async function getAccount(accountId: string) {
  const res = await fetch(`${ACCOUNTS_PATH}/${accountId}`);
  return res.json();
}

export async function listAccounts(page: number) {
  const res = await fetch(`${ACCOUNTS_PATH}?page=${page}`);
  return res.json();
}

export async function listJobs(query: string) {
  const res = await fetch(`/admin/jobs${query ? `?${query}` : ""}`);
  return res.json();
}
