import { api } from "../lib/api";

type Member = { id: string; name: string };

export async function loadMembers(): Promise<Member[]> {
  const response = await api.get<Member[]>("/members");
  return response.data;
}

export async function assignBooths(boothId: string, added: string[]): Promise<void> {
  await Promise.all(added.map((memberId) => api.post(`/booths/${boothId}/members/${memberId}`)));
}

export async function removeMember(selected: Member, reason: string): Promise<void> {
  try { await api.delete(`/members/${selected.id}`, { data: { reason } }); await loadMembers(); } catch (error) { console.error(error); }
}

export async function resetPassword(selected: Member): Promise<void> {
  try { await api.post(`/auth/reset/${selected.id}`, { notify: true }); await loadMembers(); } catch (error) { console.error(error); }
}

export async function toggleMember(boothId: string, memberId: string, assigned: boolean): Promise<void> {
  if (assigned) await api.delete(`/booths/${boothId}/members/${memberId}`); else await api.post(`/booths/${boothId}/members/${memberId}`);
}
