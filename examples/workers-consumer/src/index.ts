import escapeHtml from "escape-html";
export default {
  fetch(): Response {
    return new Response(escapeHtml("TAPID_WORKERS_ACCEPTANCE_V1"));
  },
} satisfies ExportedHandler;
