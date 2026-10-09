type Verb = "POST" | "DELETE";

export async function setMember(method: "POST" | "DELETE", groupId: string, userId: string) {
  await fetch(`/api/groups/${groupId}/members`, {
    method,
    body: JSON.stringify({ userId }),
  });
}

export async function setAliased(method: Verb, groupId: string) {
  await fetch(`/api/groups/${groupId}/aliased`, { method });
}

export async function setBound(method: "POST" | "DELETE", groupId: string) {
  const url = `/api/groups/${groupId}/bound`;
  await fetch(url, { method });
}

export async function setLoose(method: string, groupId: string) {
  await fetch(`/api/groups/${groupId}/loose`, { method });
}

export async function setOptional(groupId: string, method?: Verb) {
  await fetch(`/api/groups/${groupId}/optional`, { method });
}

export async function setReassigned(method: Verb, groupId: string) {
  method = "POST";
  await fetch(`/api/groups/${groupId}/reassigned`, { method });
}

export async function setBased(method: Verb, groupId: string) {
  await fetch(`${process.env.GROUPS_API_URL}/api/groups/${groupId}/based`, { method });
}

export async function archive(method: "POST" | "DELETE", id: string) {
  await fetch(`/api/items/${id}/archive`, { method });
}

export async function touch(method: Verb, id: string) {
  await fetch(`/api/items/${id}/touch`, { method });
}

export function nested(id: string) {
  async function send(method: "POST" | "DELETE") {
    await fetch(`/api/items/${id}/nested`, { method });
  }
  return send("POST");
}
