// A list of calls this script makes; one record carries an object, the table
// does not.
declare const USER: string;
declare function request(call: { method: string; path: string }): Promise<void>;

const CALLS = [
  { method: 'POST', path: '/session', auth: false, body: { user: USER } },
  { method: 'GET', path: '/profile', auth: true },
  { method: 'GET', path: '/profile/settings', auth: true },
];

export async function run() {
  for (const call of CALLS) {
    await request(call);
  }
}
