import { Queue } from 'bullmq';
export async function ownPush() {
  await new Queue('app-own').add('task', { n: 0 });
}
