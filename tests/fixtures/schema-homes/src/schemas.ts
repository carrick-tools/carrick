import { record, text, count } from '@fixture/checks';

/** What a caller sends to open a ticket. */
export const NewTicket = record({
  title: text(),
  priority: count(),
});
