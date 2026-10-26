/** Shows a message. Makes no request. */
export function notify(message: string): void {
  document.title = message;
}
