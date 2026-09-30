export type PollOptions = { url: string; request?: RequestInit };

export function openPoll(options: PollOptions): Promise<Response> {
  return fetch(options.url, { ...options.request, method: "GET" });
}
