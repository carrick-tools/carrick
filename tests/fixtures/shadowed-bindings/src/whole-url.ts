const answerUrl = process.env.HELPDESK_URL ?? "http://localhost:7100/api/answer";

export async function wholeUrl(local: boolean) {
  if (local) {
    const answerUrl = "/api/local-answer";
    return fetch(answerUrl, { method: "POST" });
  }
  return null;
}
