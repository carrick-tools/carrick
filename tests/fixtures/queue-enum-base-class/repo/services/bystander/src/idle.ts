import { QueueClient } from '@fx/queue-kit';

export function make() {
  return new QueueClient();
}
