import type { Metadata } from "next";

export const metadata: Metadata = {
  title: "Synthetic News Desk",
  description: "A public synthetic news-site workload for consumer acceptance checks.",
};

export default function Home() {
  return (
    <main>
      <h1>Synthetic News Desk</h1>
      <article>
        <h2>Sample headline</h2>
        <p>This page is synthetic and contains no customer or private data.</p>
      </article>
      <footer>TAPID_NEWS_SITE_ACCEPTANCE_V1</footer>
    </main>
  );
}
