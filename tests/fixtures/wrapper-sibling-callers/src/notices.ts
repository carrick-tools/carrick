function notices(query: string): Promise<Response> {
  return fetch(`/api/notices?${query}`);
}

export function unread(): Promise<Response> {
  return notices('unread=1');
}
