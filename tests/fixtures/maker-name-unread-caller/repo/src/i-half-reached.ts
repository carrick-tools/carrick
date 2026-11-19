import { Queue } from 'bullmq';
import { loadRun, type Project } from './records';

function openFor(project: Pick<Project, 'ref'>) {
  return new Queue(`halves/${project.ref}`);
}

function sendVia(queue: Queue) {
  return queue.add('event', { n: 1 });
}

export async function reached(id: string) {
  const run = await loadRun(id);
  await sendVia(openFor(run.project));
}

export async function handedIn(queue: Queue) {
  await sendVia(queue);
}
