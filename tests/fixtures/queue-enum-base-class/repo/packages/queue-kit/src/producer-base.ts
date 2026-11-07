import { Topic } from '@fx/topics';
import { QueueClient } from './queue-client';

export abstract class ProducerBase {
  constructor(public readonly topic: Topic, protected client = new QueueClient()) {
    this.client.openQueue(topic);
  }

  add(payload: unknown) {
    return this.client.add('job', payload);
  }
}
