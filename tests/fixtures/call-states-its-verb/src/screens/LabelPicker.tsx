import client from "../lib/client";

export async function addLabels(projectId: string, labelIds: string[]): Promise<void> {
  await Promise.all(labelIds.map((labelId) => client.post(`/projects/${projectId}/labels/${labelId}`)));
}

export async function renameLabels(projectId: string, labelIds: string[], name: string): Promise<void> {
  await Promise.all(labelIds.map((labelId) => client.put(`/projects/${projectId}/labels/${labelId}`, { name })));
}
