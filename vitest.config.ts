import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    // Hook/interaction tests opt into jsdom per-file via a docblock:
    //   // @vitest-environment jsdom
    environmentMatchGlobs: [
      ["src/hooks/__tests__/**", "jsdom"],
      ["src/components/__tests__/**", "jsdom"],
    ],
  },
});
