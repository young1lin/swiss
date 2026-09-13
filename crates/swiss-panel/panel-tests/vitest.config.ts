import { defineConfig } from "vitest/config";

// No setup file on purpose: the old Node-repo setup pinned MCP_GATEWAY_MASTER_KEY/HOME for the
// server suite — panel tests need neither. Anything a DOM-stub test needs, it stubs itself.
export default defineConfig({
  test: { environment: "node", include: ["test/**/*.test.ts"] },
});
