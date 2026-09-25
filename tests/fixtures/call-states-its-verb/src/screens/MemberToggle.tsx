import client from "../lib/client";

export async function toggleMember(teamId: string, memberId: string, member: boolean): Promise<void> {
  if (member) await client.delete(`/teams/${teamId}/members/${memberId}`); else await client.post(`/teams/${teamId}/members/${memberId}`);
}
