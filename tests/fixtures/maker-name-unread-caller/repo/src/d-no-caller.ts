import { Queue } from 'bullmq';
import type { Project } from './records';

export function appendUncalled(project: Pick<Project, 'ref'>) {
  const queue = new Queue(`spaces/${project.ref}`);
  return queue.add('event', { n: 1 });
}
