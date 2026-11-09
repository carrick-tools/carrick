function ping(n: number): number {
  return n > 0 ? pong(n - 1) : 0;
}

function pong(n: number): number {
  return ping(n);
}

export function start() {
  return ping(3);
}
