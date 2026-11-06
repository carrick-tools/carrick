import axios from 'axios';

const LEDGER_BASE = 'https://api.ledger.example';

export async function runJob(id: string) {
  return axios.post('https://api.ledger.example/jobs/run', { id });
}

export async function jobStatus() {
  return axios.get('https://api.ledger.example/jobs/status');
}

export async function sendMail(to: string) {
  return axios.post('https://api.mailer.example/v1/send', { to });
}
