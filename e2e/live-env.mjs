// The live-model env: the repo's .env (never committed), loaded into process.env BEFORE
// helpers.mjs snapshots it for the gateway it spawns - so import this module first. With no
// .env, or without the variables a file needs, every live file skips itself honestly.
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const file = process.env.SWISS_LIVE_ENV || join(dirname(fileURLToPath(import.meta.url)), "..", ".env");
if (existsSync(file)) process.loadEnvFile(file);

const v = (k) => (process.env[k] || "").trim();

/** One provider from SWISS_LIVE_<PREFIX>_*: base URL and model required, key optional. */
function provider(prefix) {
  const baseUrl = v(`SWISS_LIVE_${prefix}_BASE_URL`);
  const model = v(`SWISS_LIVE_${prefix}_MODEL`);
  if (!baseUrl || !model) return null;
  // The key rides as an env reference: the gateway resolves it, nothing stores the value.
  const keyVar = `SWISS_LIVE_${prefix}_API_KEY`;
  return { baseUrl, model, style: v(`SWISS_LIVE_${prefix}_STYLE`) || "openai", apiKey: v(keyVar) ? "$" + "{" + keyVar + "}" : null };
}

export const LIVE = { llm: provider("LLM"), embed: provider("EMBED") };
