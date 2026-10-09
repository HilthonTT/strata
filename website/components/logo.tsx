export function LogoMark({ className = "size-7" }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" className={className} aria-hidden>
      <defs>
        <linearGradient id="strata-mark" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#cba6f7" />
          <stop offset=".5" stopColor="#89b4fa" />
          <stop offset="1" stopColor="#94e2d5" />
        </linearGradient>
      </defs>
      <path d="M4 10.5 16 4.5l12 6-12 6z" fill="url(#strata-mark)" />
      <path d="m4 16 12 6 12-6" fill="none" stroke="url(#strata-mark)" strokeWidth="2.4" strokeLinejoin="round" opacity=".8" />
      <path d="m4 21.5 12 6 12-6" fill="none" stroke="url(#strata-mark)" strokeWidth="2.4" strokeLinejoin="round" opacity=".5" />
    </svg>
  );
}

export function Logo() {
  return (
    <span className="flex items-center gap-2.5">
      <LogoMark />
      <span className="font-mono text-lg font-semibold tracking-tight">strata</span>
    </span>
  );
}
