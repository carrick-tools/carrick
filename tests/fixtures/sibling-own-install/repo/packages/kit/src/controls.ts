import { Queue } from 'bullmq';

export function listenOnTarget(target: EventTarget) {
  const queue = new Queue('kit-listen');
  target.addEventListener('ready', () => queue.name);
}
export function listenOnWindow() {
  const channel = new BroadcastChannel('kit-window');
  window.addEventListener('ready', () => channel.name);
}
