import { Queue } from 'bullmq';
import type { Project } from './records';

declare function eachProject(cb: (project: Project) => Promise<void>): Promise<void>;

function appendEach(project: Pick<Project, 'ref'>) {
  const queue = new Queue(`each/${project.ref}`);
  return queue.add('event', { n: 1 });
}

export async function perProject() {
  await eachProject(async (project) => {
    await appendEach(project);
  });
}
