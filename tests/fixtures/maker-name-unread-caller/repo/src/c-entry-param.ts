import { Queue } from 'bullmq';
import type { Project, Run } from './records';

function appendEntry(project: Pick<Project, 'ref'>, run: Pick<Run, 'code'>) {
  const queue = new Queue(`orgs/${project.ref}/runs/${run.code}`);
  return queue.add('event', { n: 1 });
}

export async function handler(env: { project: Project }, run: Run) {
  await appendEntry(env.project, run);
}
