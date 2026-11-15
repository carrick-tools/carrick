type Send = (url: string) => Promise<Response>;

declare const flag: boolean;

async function sendBlock(url: string): Promise<Response> {
  return fetch(url);
}

async function sendPicked(url: string): Promise<Response> {
  return fetch(url);
}

export function inBlock(): Promise<Response> | undefined {
  if (flag) {
    const send: Send = sendBlock;
    return send("/v1/block");
  }
  return undefined;
}

export function declaredInBody(): Promise<Response> {
  function pick(): Send {
    return sendPicked;
  }
  return pick()("/v1/picked");
}

export function calledByName(): Promise<Response> {
  function local(url: string): Promise<Response> {
    return fetch(url);
  }
  return local("/v1/local");
}

export function insideCallback(senders: Send[]): void {
  senders.forEach((one) => {
    if (flag) {
      const held = one;
      held("/v1/callback");
    }
  });
}

export function outer(send: Send, other: Send): void {
  [1].forEach(() => {
    let s = send;
    if (flag) s = other;
    s("/v1/outer");
  });
}

export function walkAll(): void {
  function visit(n: number): void {
    if (n > 0) visit(n - 1);
  }
  visit(3);
}

export function objectInBlock(): void {
  if (flag) {
    const held = { send: sendBlock };
    const again = held;
    again.send("/v1/object");
  }
}
