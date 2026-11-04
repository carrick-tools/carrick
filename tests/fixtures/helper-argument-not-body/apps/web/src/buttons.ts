import { trackClick } from "./track";

export function onButton(button: { id: string }): void {
  trackClick(button.id);
}

export function onButtonNow(button: { id: string }): void {
  trackClick(button.id, true);
}

export function pingApi(): Promise<Response> {
  return fetch("/ping");
}
