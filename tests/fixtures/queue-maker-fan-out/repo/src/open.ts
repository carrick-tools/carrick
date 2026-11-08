import { Queue } from 'bullmq';

export function open(name: string) {
  return new Queue(name);
}

export async function send(name: string) {
  return open(name).add('job', { n: 1 });
}
