/* Open pages follow explicit choices, never boot or a received relay (SPEC §panel.i18n).
 * Storage and rendering stay with i18n.ts and main.ts; the hub stores no preferences. */
import { busOn, busRelay } from "./bus.js";
import { create } from "./codec.js";
import { Module } from "./pb/swiss/bus/v1/bus_pb.js";
import { PanelSub, PrefsId, PrefsSchema } from "./pb/swiss/bus/v1/panel_pb.js";
import { CODE_SCHEMES } from "../ui/code-scheme.js";
import type { Lang } from "../i18n.js";

type Theme = "light" | "dark" | "auto";
interface Choices { lang?: Lang; theme?: Theme; codeScheme?: string }
interface ApplyPrefs {
  lang: (lang: Lang) => Promise<void>;
  theme: (theme: Theme) => void;
  codeScheme: (scheme: string) => void;
}
const route = { module: Module.PANEL, sub: PanelSub.PREFS, id: PrefsId.PREFS_CHANGED, schema: PrefsSchema };

export function relayPrefs(choice: Choices): void {
  busRelay(route, "", create(PrefsSchema, choice));
}

export function initPrefs(apply: ApplyPrefs): () => void {
  return busOn({ module: route.module, sub: route.sub, key: "" }, route.id, PrefsSchema, (prefs) => {
    if (prefs.theme === "light" || prefs.theme === "dark" || prefs.theme === "auto") apply.theme(prefs.theme);
    if (prefs.codeScheme !== undefined && CODE_SCHEMES.includes(prefs.codeScheme)) apply.codeScheme(prefs.codeScheme);
    if (prefs.lang === "en" || prefs.lang === "zh-CN") void apply.lang(prefs.lang);
  });
}
