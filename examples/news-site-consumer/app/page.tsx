import Link from "next/link";

const articles = [
  { title: "Local library opens a new community newsroom", category: "Community" },
  { title: "Harbor cleanup brings volunteers together", category: "Environment" },
];

export default function Home() {
  return (
    <main>
      <h1>Synthetic daily briefing</h1>
      <p>Independent local reporting for the community.</p>
      <ul>
        {articles.map((article) => (
          <li key={article.title}>
            <strong>{article.category}</strong>: {article.title}
          </li>
        ))}
      </ul>
      <Link href="/acceptance">Acceptance check</Link>
    </main>
  );
}
