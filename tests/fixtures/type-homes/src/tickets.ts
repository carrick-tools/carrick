import WebSocket from 'ws';
import type { TicketUpdate } from './models';

const socket = new WebSocket('ws://localhost:8080');

const handlers = {
  'ticket:updated': (update: TicketUpdate) => {
    void update;
  },
};

socket.on('message', (raw: string) => {
  const msg = JSON.parse(raw);
  handlers[msg.type]?.(msg.payload);
});
