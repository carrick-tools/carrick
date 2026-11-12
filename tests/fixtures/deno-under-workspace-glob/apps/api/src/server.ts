import express from 'express';
export const app = express();
app.get('/status', (_req, res) => res.json({ ready: true }));
