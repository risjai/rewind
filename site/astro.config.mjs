import { defineConfig } from "astro/config";
import mdx from "@astrojs/mdx";
import sitemap from "@astrojs/sitemap";
import tailwind from "@astrojs/tailwind";
import vercel from "@astrojs/vercel";

export default defineConfig({
  site: "https://agentoptics.dev",
  output: "static",
  adapter: vercel(),
  integrations: [mdx(), sitemap(), tailwind()],
});
