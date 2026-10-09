import { Info, TriangleAlert } from "lucide-react";

export function Callout({ type = "info", children }: { type?: "info" | "warn"; children: React.ReactNode }) {
  const warn = type === "warn";
  const Icon = warn ? TriangleAlert : Info;
  return (
    <div
      className={`not-prose my-6 flex gap-3 rounded-xl border p-4 text-sm leading-relaxed ${
        warn ? "border-layer-peach/30 bg-layer-peach/5 text-mist-100" : "border-layer-sky/30 bg-layer-sky/5 text-mist-100"
      }`}
    >
      <Icon className={`mt-0.5 size-4 shrink-0 ${warn ? "text-layer-peach" : "text-layer-sky"}`} />
      <div className="[&_code]:rounded [&_code]:bg-white/10 [&_code]:px-1 [&_code]:font-mono">{children}</div>
    </div>
  );
}

export function Kbd({ children }: { children: React.ReactNode }) {
  return (
    <kbd className="rounded-md border border-white/15 border-b-white/25 bg-ink-800 px-1.5 py-0.5 font-mono text-[0.8em] text-mist-100">
      {children}
    </kbd>
  );
}
