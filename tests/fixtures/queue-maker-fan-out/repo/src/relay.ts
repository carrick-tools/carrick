import { send } from './open';

// Seven levels of two helpers, each calling both helpers of the level
// below: 128 paths from `main` down to `send`, all with one name.

export async function a1(name: string) {
  return send(name);
}
export async function a2(name: string) {
  return send(name);
}

export async function b1(name: string) {
  await a1(name);
  return a2(name);
}
export async function b2(name: string) {
  await a1(name);
  return a2(name);
}

export async function c1(name: string) {
  await b1(name);
  return b2(name);
}
export async function c2(name: string) {
  await b1(name);
  return b2(name);
}

export async function d1(name: string) {
  await c1(name);
  return c2(name);
}
export async function d2(name: string) {
  await c1(name);
  return c2(name);
}

export async function e1(name: string) {
  await d1(name);
  return d2(name);
}
export async function e2(name: string) {
  await d1(name);
  return d2(name);
}

export async function f1(name: string) {
  await e1(name);
  return e2(name);
}
export async function f2(name: string) {
  await e1(name);
  return e2(name);
}

export async function g1(name: string) {
  await f1(name);
  return f2(name);
}
export async function g2(name: string) {
  await f1(name);
  return f2(name);
}

export async function main() {
  await g1('fan-jobs');
  return g2('fan-jobs');
}
