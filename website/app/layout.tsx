import type { Metadata } from "next";
import { Inter, JetBrains_Mono } from "next/font/google";

import { asset, site } from "@/lib/site";
import "./globals.css";

const inter = Inter({ subsets: ["latin"], variable: "--font-inter" });
const jetbrains = JetBrains_Mono({ subsets: ["latin"], variable: "--font-jetbrains" });

export const metadata: Metadata = {
  title: { default: `${site.name} — ${site.tagline}`, template: `%s · ${site.name}` },
  description:
    "strata is a terminal file explorer written in Rust: multiple panels, previews, git status, undo, NAS connections, Docker, a disk dashboard, 27 themes and Lua plugins.",
  icons: { icon: asset("/favicon.svg") },
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className={`${inter.variable} ${jetbrains.variable}`}>
      <body className="min-h-screen font-sans antialiased">{children}</body>
    </html>
  );
}
