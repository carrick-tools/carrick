import { Queue, Worker } from 'bullmq';
import { Topic } from './topics';

const direct = new Queue(Topic.Beta);

export async function c1Send() {
  return direct.add('direct-job', { n: 1 });
}

export const c1Worker = new Worker(Topic.Beta, async (job) => job.data);
