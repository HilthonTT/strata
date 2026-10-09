export const site = {
  name: "strata",
  tagline: "A fast, extensible terminal file explorer",
  repo: "https://github.com/HilthonTT/strata",
  install: "cargo install --locked --git https://github.com/HilthonTT/strata strata",
};

/** Prefixes public assets with the deploy base path (GitHub Pages). */
export function asset(path: string): string {
  const base = process.env.NEXT_PUBLIC_BASE_PATH ?? "";
  return `${base}${path.startsWith("/") ? path : `/${path}`}`;
}
