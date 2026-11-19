import { Queue } from 'bullmq';
import { loadRun } from './records';

function openNamed(name: string) {
  return new Queue(name);
}

export async function fieldA(id: string) {
  const run = await loadRun(id);
  await openNamed(run.code).add('event', { n: 1 });
}

export async function fieldB(id: string) {
  const run = await loadRun(id);
  await openNamed(run.code).add('event', { n: 1 });
}
