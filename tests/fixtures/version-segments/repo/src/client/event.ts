import { Live } from '@fixture/live-wire';

type O = { event?: string };

export function announce(payload: unknown, options?: O) {
  const { event = 'room.versioned' } = options || {};
  new Live('wss://live.example/ws').emit(event, payload);
}
