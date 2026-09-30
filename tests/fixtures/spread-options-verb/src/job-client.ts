import { openPoll } from "./poll.js";

export function watchJob(apiUrl: string, id: string) {
  return openPoll({
    url: `${apiUrl}/api/v1/jobs/${id}`,
    request: { method: "DELETE" },
  });
}
