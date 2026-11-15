import { Queue } from 'bullmq';
import { loadRun, type Project, type Run } from './records';

function appendMix(project: Pick<Project, 'ref'>, run: Pick<Run, 'code'>) {
  const queue = new Queue(`mixed/${project.ref}/runs/${run.code}`);
  return queue.add('event', { n: 1 });
}

export async function mixLiteral() {
  await appendMix({ ref: 'acme' }, { code: 'r1' });
}

export async function mixRecord(id: string) {
  const run = await loadRun(id);
  await appendMix(run.project, run);
}
