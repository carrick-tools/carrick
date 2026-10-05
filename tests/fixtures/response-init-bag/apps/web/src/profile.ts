import { request } from "an-http-client";
import { sendJson, type Parser } from "@fixture/http-kit";

const PROFILE_URL = "/api/profile";
const ORDERS_URL = "/api/orders";
const PREFERENCES_URL = "/api/preferences";
const NOTICES_URL = "/api/notices";
const ACCOUNT_URL = "/api/account";

interface Preferences {
  theme: string;
}

const preferencesParser: Parser<Preferences> = {
  parse: (value) => value as Preferences,
};

export async function loadProfile(token: string) {
  const response = await request(PROFILE_URL, {
    headers: { Authorization: token },
  });
  return response.json();
}

export function loadOrders(token: string) {
  return request(ORDERS_URL, { headers: { Authorization: token } }).then(
    (response) => response.json(),
  );
}

export function loadPreferences(token: string) {
  return sendJson(preferencesParser, PREFERENCES_URL, {
    headers: { Authorization: token },
  });
}

export function loadNotices(token: string) {
  return fetch(NOTICES_URL, { headers: { Authorization: token } });
}

export function loadAccount(token: string) {
  return request(ACCOUNT_URL, { headers: { Authorization: token } });
}
