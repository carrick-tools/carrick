import express from 'express';

const app = express();

app.get('/api/v1/environments/', (_req, res) => {
  res.json({ environments: ['staging', 'production'] });
});

app.get('/', (_req, res) => {
  res.json({ ok: true });
});

app.listen(3000);
