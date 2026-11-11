import { bus } from '@fixture/queue';

type O = { topic?: string };

export const publish = async (payload: unknown, options?: O) => {
  const { topic = 'orders.versioned' } = options || {};
  await bus.publish(topic, payload);
};
