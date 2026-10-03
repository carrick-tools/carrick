import express from 'express';
import type { Report } from './contracts';

export const app = express();

app.get('/reports/:id', (req, res) => {
  const report = { reportId: req.params.id } as Report;
  res.json(report);
});
