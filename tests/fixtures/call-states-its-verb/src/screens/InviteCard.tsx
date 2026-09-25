import client from "../lib/client";

type Account = { id: string; email: string };

export async function resendInvite(account: Account, setBusy: (busy: boolean) => void): Promise<string | undefined> {
  try { setBusy(true); const res = await client.post(`/accounts/${account.id}/invite`, { note: "resent" }); return res.data.token; } catch (error) { console.error(error); } finally { setBusy(false); }
}

export async function renameAccount(account: Account, name: string, setBusy: (busy: boolean) => void): Promise<void> {
  try { setBusy(true); await client.patch(`/accounts/${account.id}`, { name }); } catch (error) { console.error(error); } finally { setBusy(false); }
}
