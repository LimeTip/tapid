import { Hono } from "hono";

export const acceptanceMarker = "TAPID_NEWS_SITE_ACCEPTANCE_V1";
export const app = new Hono();

const articles = [
  { title: "Local library opens a new community newsroom", category: "Community" },
  { title: "Harbor cleanup brings volunteers together", category: "Environment" },
];

app.get("/", (context) =>
  context.html(`<!doctype html>
<html lang="en">
  <head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Daily Briefing</title></head>
  <body>
    <main>
      <h1>Synthetic daily briefing</h1>
      <p>Independent local reporting for the community.</p>
      <ul>${articles.map((article) => `<li><strong>${article.category}</strong>: ${article.title}</li>`).join("")}</ul>
    </main>
  </body>
</html>`),
);

app.get("/acceptance", (context) => context.text(acceptanceMarker));
