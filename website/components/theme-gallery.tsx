"use client";

import { useState } from "react";

import { asset } from "@/lib/site";

const themes = [
  { id: "catppuccin-mocha", label: "Catppuccin Mocha", file: "main.png", swatch: ["#1e1e2e", "#cba6f7", "#89b4fa"] },
  { id: "tokyo-night", label: "Tokyo Night", file: "theme-tokyo-night.png", swatch: ["#1a1b26", "#7aa2f7", "#bb9af7"] },
  { id: "nord", label: "Nord", file: "theme-nord.png", swatch: ["#2e3440", "#88c0d0", "#81a1c1"] },
  { id: "gruvbox-dark", label: "Gruvbox", file: "theme-gruvbox-dark.png", swatch: ["#282828", "#fe8019", "#b8bb26"] },
  { id: "rose-pine", label: "Rosé Pine", file: "theme-rose-pine.png", swatch: ["#191724", "#c4a7e7", "#ebbcba"] },
  { id: "dracula", label: "Dracula", file: "theme-dracula.png", swatch: ["#282a36", "#bd93f9", "#ff79c6"] },
  { id: "catppuccin-latte", label: "Catppuccin Latte", file: "theme-catppuccin-latte.png", swatch: ["#eff1f5", "#8839ef", "#1e66f5"] },
];

export function ThemeGallery() {
  const [active, setActive] = useState(themes[0]);
  return (
    <div className="grid gap-8 lg:grid-cols-[17rem_1fr]">
      <div className="flex gap-2 overflow-x-auto pb-2 lg:flex-col lg:overflow-visible lg:pb-0">
        {themes.map((t) => (
          <button
            key={t.id}
            type="button"
            onClick={() => setActive(t)}
            className={`flex shrink-0 items-center gap-3 rounded-xl border px-4 py-3 text-left text-sm transition ${
              active.id === t.id
                ? "border-layer-violet/50 bg-white/[0.06] text-white"
                : "border-white/5 text-mist-300 hover:border-white/15 hover:text-white"
            }`}
          >
            <span className="flex">
              {t.swatch.map((c, i) => (
                <span
                  key={c}
                  className="size-4 rounded-full ring-2 ring-ink-900"
                  style={{ background: c, marginLeft: i === 0 ? 0 : -6 }}
                />
              ))}
            </span>
            {t.label}
          </button>
        ))}
        <p className="hidden px-1 pt-2 text-sm text-mist-400 lg:block">
          …and 20 more, plus your own in <code className="font-mono text-mist-300">themes/*.toml</code>.
        </p>
      </div>
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        key={active.id}
        src={asset(`/media/${active.file}`)}
        alt={`strata with the ${active.label} theme`}
        className="w-full rounded-2xl shadow-2xl ring-1 shadow-black/50 ring-white/10"
      />
    </div>
  );
}
