export interface CloseInput {
  jobId: string;
}

export interface OpenInput {
  id: string;
}

export interface OpenReply {
  opened: boolean;
}

export async function closeJob(jobId: string) {
  const input: CloseInput = { jobId };
  const response = await fetch("/v1/jobs/close", { method: "POST", body: JSON.stringify(input) });
  return response.json();
}

export async function openJob(id: string): Promise<OpenReply> {
  const input: OpenInput = { id };
  const response = await fetch("/v1/jobs/open", { method: "POST", body: JSON.stringify(input) });
  const reply: OpenReply = await response.json();
  return reply;
}
