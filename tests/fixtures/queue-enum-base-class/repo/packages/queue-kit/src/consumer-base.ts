import { Processor } from 'bullmq';
import { Topic } from '@fx/topics';
import { QueueClient } from './queue-client';

export abstract class ConsumerBase {
  constructor(public readonly topic: Topic, protected client = new QueueClient()) {}

  init(p: Processor) {
    this.client.openConsumer(this.topic, p);
  }
}
