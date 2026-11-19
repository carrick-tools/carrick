import { Queue } from 'bullmq';
import { loadRun, type Project } from './records';

class BuildStream {
  private queue: Queue;
  constructor(project: Pick<Project, 'ref'>) {
    this.queue = new Queue(`builds/${project.ref}`);
  }
  send() {
    return this.queue.add('event', { n: 1 });
  }
}

export async function viaCtor(id: string) {
  const run = await loadRun(id);
  await new BuildStream(run.project).send();
}
