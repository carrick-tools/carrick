import { listenOnTarget, listenOnWindow } from '@fx/kit';
export function relayTarget(target: EventTarget) { return listenOnTarget(target); }
export function relayWindow() { return listenOnWindow(); }
