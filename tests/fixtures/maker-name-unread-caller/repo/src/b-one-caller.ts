import { Queue } from 'bullmq';
import { loadRun, type Project, type Run } from './records';

function appendOne(project: Pick<Project, 'ref'>, run: Pick<Run, 'code'>) {
  const queue = new Queue(`teams/${project.ref}/runs/${run.code}`);
  return queue.add('event', { n: 1 });
}

export async function onlyCaller(id: string) {
  const run = await loadRun(id);
  await appendOne(run.project, run);
}
