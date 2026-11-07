import { Queue, Worker, Processor } from 'bullmq';
import { Topic } from '@fx/topics';

const connection = { host: 'localhost', port: 6379 };

export class QueueClient {
  private queueRef?: Queue;
  private consumerRef?: Worker;

  openQueue(topic: Topic) {
    this.queueRef = new Queue(topic, { connection });
    return this.queueRef;
  }

  openConsumer(topic: Topic, p: Processor) {
    this.consumerRef = new Worker(topic, p, { connection });
    return this.consumerRef;
  }

  add(name: string, data: unknown) {
    return this.queueRef!.add(name, data);
  }
}
