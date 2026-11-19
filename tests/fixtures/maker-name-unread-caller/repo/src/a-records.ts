import { Queue } from 'bullmq';
import { loadRun, type Project, type Run } from './records';

export class EventLog {
  append(project: Pick<Project, 'ref'>, run: Pick<Run, 'code'>, events: string[]) {
    const queue = new Queue(`projects/${project.ref}/runs/${run.code}`);
    return queue.add('event', { events });
  }

  async restart(id: string) {
    const run = await loadRun(id);
    await this.append(run.project, run, ['restart']);
  }
}

export async function onStart(id: string) {
  const run = await loadRun(id);
  await new EventLog().append(run.project, run, ['start']);
}

export async function onStop(id: string) {
  const run = await loadRun(id);
  await new EventLog().append(run.project, run, ['stop']);
}
