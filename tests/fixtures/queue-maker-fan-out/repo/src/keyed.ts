import { Queue } from 'bullmq';
const q = new Queue('emails');
q.add('static', { n: 1 });
const M = 'add' as const;
q[M]('keyed', { n: 2 });
for (const m of ['add'] as const) q[m]('looped', {});
