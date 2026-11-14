import { bus } from '@fixture/queue';
import { WATCHED } from './watched';

export function listenAll(): void {
  for (const feed of WATCHED) {
    bus.subscribe(feed, () => {});
  }
}
