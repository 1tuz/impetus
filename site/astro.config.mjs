import { defineConfig } from "astro/config";

const isPagesBuild = process.env.GITHUB_ACTIONS === "true";

export default defineConfig({
  output: "static",
  base: isPagesBuild ? "/impetus" : "/",
  trailingSlash: "always",
});
