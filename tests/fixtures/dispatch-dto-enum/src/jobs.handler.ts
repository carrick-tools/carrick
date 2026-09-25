type JobRequest = { action: string; id: string };

export async function handler(event: { body: string }): Promise<{ statusCode: number; body: string }> {
  const request = JSON.parse(event.body) as JobRequest;
  switch (request.action) {
    case "archive":
      await fetch(`https://store.example.com/v1/jobs/${request.id}/archive`, { method: "POST" });
      return { statusCode: 200, body: "archived" };
    case "publish":
      await fetch(`https://store.example.com/v1/jobs/${request.id}/publish`, { method: "POST" });
      return { statusCode: 200, body: "published" };
    default:
      return { statusCode: 400, body: "unknown action" };
  }
}
