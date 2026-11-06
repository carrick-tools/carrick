import express from 'express';

const app = express();

app.post('/jobs/run', (req, res) => {
  res.json({ started: true });
});

app.get('/jobs/status', (req, res) => {
  res.json({ running: 0 });
});

// Probe: a route the undeclared mailer host's path would also fit.
app.post('/v1/send', (req, res) => {
  res.json({ queued: true });
});

app.listen(3000);
