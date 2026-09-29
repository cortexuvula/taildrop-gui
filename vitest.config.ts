import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    // Vitest must not discover Playwright's e2e/*.test.ts files — importing
    // them outside the Playwright runner throws "test() called here" and
    // fails the whole suite. They run via `npm run test:e2e` instead.
    include: ["src/**/*.test.{ts,tsx}"],
    // Hook/interaction tests opt into jsdom per-file via a docblock:
    //   // @vitest-environment jsdom
    environmentMatchGlobs: [
      ["src/hooks/__tests__/**", "jsdom"],
      ["src/components/__tests__/**", "jsdom"],
    ],
  },
});
