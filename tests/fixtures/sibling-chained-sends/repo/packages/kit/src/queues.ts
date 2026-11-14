import { Queue } from 'bullmq';

export async function pushLiteral() {
  await new Queue('kit-literal').add('task', { n: 1 });
}
export async function pushHeld() {
  const held = new Queue('kit-held');
  await held.add('task', { n: 1 });
}
export async function pushBoth() {
  const kept = new Queue('kit-kept');
  await kept.add('task', { n: 1 });
  await new Queue('kit-chained').add('task', { n: 2 });
}
export function openOnly(name: string) {
  return new Queue(name);
}
export async function closeChained() {
  await new Queue('kit-closed').close();
}
