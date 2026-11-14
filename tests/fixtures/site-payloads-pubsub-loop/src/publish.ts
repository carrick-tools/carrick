import { bus } from '@fixture/queue';

const FEEDS = ['feed.created', 'feed.updated'] as const;

export async function announceAll(): Promise<void> {
  for (const feed of FEEDS) {
    await bus.publish(feed, { feed });
  }
}
