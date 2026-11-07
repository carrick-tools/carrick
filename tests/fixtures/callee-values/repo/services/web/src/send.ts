export function send(path: string) {
  return fetch(`https://api.example.test${path}`);
}

export function sendToo(path: string) {
  return fetch(`https://api.example.test/v2${path}`);
}

export function other(path: string) {
  return path;
}
