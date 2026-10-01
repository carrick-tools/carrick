const cache = new Map<string, string>();

export function read() {
  return cache.get("/r");
}

export function readInPlace() {
  return new Map<string, string>().get("/r");
}
