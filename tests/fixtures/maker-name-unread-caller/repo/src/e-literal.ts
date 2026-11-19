import { Queue } from 'bullmq';
import type { Project, Run } from './records';

function appendLit(project: Pick<Project, 'ref'>, run: Pick<Run, 'code'>) {
  const queue = new Queue(`lits/${project.ref}/runs/${run.code}`);
  return queue.add('event', { n: 1 });
}

export async function stated() {
  await appendLit({ ref: 'acme' }, { code: 'r1' });
}
