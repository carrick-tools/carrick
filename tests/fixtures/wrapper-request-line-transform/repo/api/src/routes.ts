import express from 'express';
import type { Note, NoteBody, Ticket, Token, TokenBody } from './types';

const app = express();
app.use(express.json());

app.get('/v1/tickets', (req: unknown, res: { json(value: Ticket[]): void }) => {
  const tickets: Ticket[] = [{ id: 't-1', title: 'first' }];
  res.json(tickets);
});

app.get('/v1/tickets/latest', (req: unknown, res: { json(value: Ticket): void }) => {
  const ticket: Ticket = { id: 't-2', title: 'latest' };
  res.json(ticket);
});

app.post('/v1/tokens', (req: { body: TokenBody }, res: { json(value: Token): void }) => {
  const token: Token = { token: req.body.scopes.join(',') };
  res.json(token);
});

app.post('/v1/notes', (req: { body: NoteBody }, res: { json(value: Note): void }) => {
  const note: Note = { id: 'n-1', text: req.body.text };
  res.json(note);
});

app.post('/v1/accounts/:account/tokens', (req: { body: TokenBody }, res: { json(value: Token): void }) => {
  const token: Token = { token: req.body.scopes.join(',') };
  res.json(token);
});

app.listen(4100);
