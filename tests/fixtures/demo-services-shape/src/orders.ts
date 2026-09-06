import axios from "axios";
import { User } from "./users.controller";

// The origin comes from the environment, with a local default. The path is
// written at the call site.
const USER_SERVICE_URL = process.env.USER_SERVICE_URL || "http://localhost:3001";

export async function enrich(orderId: string, userId: string): Promise<User> {
  const user = await axios.get<User>(`${USER_SERVICE_URL}/api/users/${userId}`);
  return user.data;
}

export async function rename(userId: string, name: string): Promise<User> {
  const renamed = await axios.post<User>(
    `${USER_SERVICE_URL}/api/users/${userId}/rename`,
    { name },
  );
  return renamed.data;
}

// The same statement written as a concatenation.
export async function listUsers(): Promise<User[]> {
  const users = await axios.get<User[]>(USER_SERVICE_URL + "/api/users");
  return users.data;
}

// The environment read at the call site, with no binding in between.
export async function health(): Promise<string> {
  const status = await fetch(`${process.env.USER_SERVICE_URL}/api/health`, {
    method: "GET",
  });
  return status.text();
}
