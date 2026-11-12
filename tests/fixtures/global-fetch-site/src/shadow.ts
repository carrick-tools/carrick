function fetch(url: string): number {
  return url.length;
}

export function measure(): number {
  return fetch("/x");
}
