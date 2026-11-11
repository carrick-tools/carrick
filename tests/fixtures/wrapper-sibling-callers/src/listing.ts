function page(path: string, query: string): Promise<Response> {
  return fetch(`${path}?${query}`);
}

function listing(query: string): Promise<Response> {
  return page('/api/listing', query);
}

export function unreadListing(): Promise<Response> {
  return listing('unread=1');
}
