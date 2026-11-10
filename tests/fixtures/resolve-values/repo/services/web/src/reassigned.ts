type Send = (url: string) => Promise<Response>;

async function sendA(url: string): Promise<Response> {
  return fetch(url);
}

let send: Send = sendA;
[send] = [sendA];

export function run(s: Send = send): Promise<Response> {
  return s("/v1/run");
}
