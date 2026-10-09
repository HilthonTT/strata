import Link from "next/link";
import {
  ArrowRight,
  Boxes,
  Container,
  FileSearch,
  GitBranch,
  HardDrive,
  Image as ImageIcon,
  Keyboard,
  Layers,
  Network,
  Palette,
  Puzzle,
  Undo2,
} from "lucide-react";

import { CopyCommand } from "@/components/copy-command";
import { GithubIcon } from "@/components/github-icon";
import { Media } from "@/components/media";
import { SiteFooter } from "@/components/site-footer";
import { SiteHeader } from "@/components/site-header";
import { ThemeGallery } from "@/components/theme-gallery";
import { site } from "@/lib/site";

const strata = ["#cba6f7", "#b4befe", "#89b4fa", "#74c7ec", "#94e2d5"];

/** Soft horizontal bands, a nod to geological strata. */
function Bands() {
  return (
    <div
      aria-hidden
      className="pointer-events-none absolute inset-x-0 top-0 -z-10 h-[56rem] overflow-hidden [mask-image:linear-gradient(to_bottom,black_45%,transparent)]"
    >
      <div className="bg-grid absolute inset-0" />
      {strata.map((c, i) => (
        <div
          key={c}
          className="absolute left-1/2 h-40 w-[140%] -translate-x-1/2 rounded-[100%] blur-3xl"
          style={{
            top: `${6 + i * 7}rem`,
            background: c,
            opacity: 0.07 + (4 - i) * 0.012,
            transform: `translateX(-50%) rotate(${-4 + i * 2}deg)`,
          }}
        />
      ))}
    </div>
  );
}

function Section({
  id,
  eyebrow,
  title,
  intro,
  children,
}: {
  id?: string;
  eyebrow: string;
  title: React.ReactNode;
  intro?: string;
  children: React.ReactNode;
}) {
  return (
    <section id={id} className="mx-auto max-w-7xl scroll-mt-24 px-5 py-24">
      <p className="font-mono text-xs uppercase tracking-[0.25em] text-layer-violet">
        {eyebrow}
      </p>
      <h2 className="mt-3 max-w-3xl text-3xl font-semibold tracking-tight text-white sm:text-4xl">
        {title}
      </h2>
      {intro && (
        <p className="mt-4 max-w-2xl text-lg leading-relaxed text-mist-300">
          {intro}
        </p>
      )}
      <div className="mt-12">{children}</div>
    </section>
  );
}

function Showcase({
  title,
  text,
  media,
  alt,
  icon: Icon,
  className = "",
}: {
  title: string;
  text: string;
  media: string;
  alt: string;
  icon: React.ComponentType<{ className?: string }>;
  className?: string;
}) {
  return (
    <div
      className={`group flex flex-col overflow-hidden rounded-3xl border border-white/[0.07] bg-ink-850 ${className}`}
    >
      <div className="p-7 pb-0">
        <span className="inline-flex rounded-xl bg-white/5 p-2.5 text-layer-sky ring-1 ring-white/10">
          <Icon className="size-5" />
        </span>
        <h3 className="mt-4 text-lg font-semibold text-white">{title}</h3>
        <p className="mt-1.5 text-sm leading-relaxed text-mist-300">{text}</p>
      </div>
      <div className="mt-6 flex-1 px-3 pb-3">
        <Media
          src={media}
          alt={alt}
          className="transition duration-500 group-hover:scale-[1.01]"
        />
      </div>
    </div>
  );
}

const capabilities = [
  {
    icon: GitBranch,
    title: "Git aware",
    text: "Modified, added, untracked and ignored files are marked right in the panel.",
  },
  {
    icon: Undo2,
    title: "Undo",
    text: "Renames, moves, copies, new files and trashing can all be undone with u.",
  },
  {
    icon: Layers,
    title: "Panels & tabs",
    text: "Up to six panels side by side, and tabs to keep whole workspaces apart.",
  },
  {
    icon: ImageIcon,
    title: "Rich preview",
    text: "Syntax-highlighted code, images over kitty, sixel or iTerm2, and archives.",
  },
  {
    icon: Container,
    title: "Docker",
    text: "Start, stop and inspect containers, read logs, or browse their filesystem.",
  },
  {
    icon: Keyboard,
    title: "Your keys",
    text: "Vim-style by default, a classic ctrl+c / ctrl+v preset, and every key remappable.",
  },
  {
    icon: Palette,
    title: "27 themes",
    text: "Catppuccin, Nord, Tokyo Night, Gruvbox, Rosé Pine, Dracula and more.",
  },
  {
    icon: Puzzle,
    title: "Lua plugins",
    text: "Add keys, commands, status segments, panels and previewers in a few lines.",
  },
];

export default function Home() {
  return (
    <>
      <SiteHeader />
      <main className="relative isolate">
        <Bands />

        {/* Hero */}
        <section className="mx-auto max-w-7xl px-5 pt-20 pb-12 text-center sm:pt-28">
          <Link
            href="/docs/"
            className="inline-flex items-center gap-2 rounded-full border border-white/10 bg-white/[0.03] px-4 py-1.5 text-xs text-mist-300 transition hover:border-white/25 hover:text-white"
          >
            <span className="size-1.5 rounded-full bg-layer-teal" />
            Written in Rust · runs on Linux, macOS and Windows
            <ArrowRight className="size-3" />
          </Link>
          <h1 className="mx-auto mt-8 max-w-4xl text-5xl font-semibold tracking-tight text-white sm:text-7xl">
            Your files, <span className="text-layers">in layers.</span>
          </h1>
          <p className="mx-auto mt-6 max-w-2xl text-lg leading-relaxed text-mist-300 sm:text-xl">
            strata is a fast terminal file explorer with side-by-side panels,
            rich previews, git status, undo, NAS connections, Docker and a disk
            dashboard. Everything you need, nothing you don&apos;t.
          </p>
          <div className="mt-10 flex flex-col items-center gap-4">
            <CopyCommand command={site.install} />
            <div className="flex flex-wrap justify-center gap-3">
              <Link
                href="/docs/installation/"
                className="inline-flex items-center gap-2 rounded-xl bg-white px-5 py-2.5 text-sm font-semibold text-ink-900 transition hover:bg-mist-100"
              >
                Get started <ArrowRight className="size-4" />
              </Link>
              <a
                href={site.repo}
                className="inline-flex items-center gap-2 rounded-xl border border-white/10 px-5 py-2.5 text-sm font-semibold text-white transition hover:border-white/30"
              >
                <GithubIcon /> Star on GitHub
              </a>
            </div>
          </div>
        </section>

        <div className="relative mx-auto max-w-6xl px-5">
          <div
            aria-hidden
            className="absolute inset-x-16 top-10 -z-10 h-[80%] rounded-full bg-gradient-to-r from-layer-violet/25 via-layer-sky/20 to-layer-teal/25 blur-3xl"
          />
          <Media
            src="overview.gif"
            alt="strata browsing a project: previews, filtering and the dashboard"
            priority
          />
        </div>

        {/* Showcase */}
        <Section
          id="features"
          eyebrow="Features"
          title={
            <>
              Everything a file manager should do,{" "}
              <span className="text-layers">right in your terminal</span>.
            </>
          }
          intro="Copy, move, rename and delete in the background with live progress. Search names or contents. Keep an eye on your disks. Reach your NAS and containers without leaving the keyboard."
        >
          <div className="grid gap-5 lg:grid-cols-2">
            <Showcase
              icon={Boxes}
              title="File operations that keep up"
              text="Mark files, copy them to the next panel, rename in bulk with your editor — and undo any of it."
              media="file-operations.gif"
              alt="Copying between panels, renaming and undoing in strata"
            />
            <Showcase
              icon={FileSearch}
              title="Find anything"
              text="Fuzzy-find files across a project, or search inside them with ripgrep and jump straight to the line."
              media="search.gif"
              alt="Fuzzy finding and content search in strata"
            />
            <Showcase
              icon={HardDrive}
              title="A dashboard for your disks"
              text="Free space, IOPS, throughput and latency per device, memory pressure from PSI, and what is eating your space."
              media="dashboard.png"
              alt="The strata dashboard"
            />
            <Showcase
              icon={Network}
              title="NAS connections, handled"
              text="SMB, NFS and SFTP with live reachability, a step-by-step connection check, and passwords kept in your system keychain."
              media="nas.png"
              alt="NAS connections in strata"
            />
          </div>

          <div className="mt-5 grid gap-px overflow-hidden rounded-3xl border border-white/[0.07] bg-white/[0.07] sm:grid-cols-2 lg:grid-cols-4">
            {capabilities.map(({ icon: Icon, title, text }) => (
              <div key={title} className="bg-ink-850 p-7">
                <Icon className="size-5 text-layer-violet" />
                <h3 className="mt-4 font-semibold text-white">{title}</h3>
                <p className="mt-1.5 text-sm leading-relaxed text-mist-300">
                  {text}
                </p>
              </div>
            ))}
          </div>
        </Section>

        {/* Themes */}
        <Section
          id="themes"
          eyebrow="Themes"
          title="Looks right in any terminal."
          intro="Pick a theme with T and watch it change as you scroll. Custom themes are a dozen hex codes, and can inherit from a built-in one."
        >
          <ThemeGallery />
        </Section>

        {/* Plugins */}
        <Section
          eyebrow="Plugins"
          title="Make it yours with a few lines of Lua."
          intro="Plugins can bind keys, add commands, react to events, draw their own panels and preview new file types. git, bookmarks, archive and zoxide ship with strata."
        >
          <div className="grid items-center gap-10 lg:grid-cols-2">
            <pre className="overflow-x-auto rounded-3xl border border-white/[0.07] bg-ink-950 p-7 font-mono text-[0.85rem] leading-7 text-mist-300">
              <code>
                <span className="text-mist-400">
                  -- ~/.config/strata/plugins/projects.lua
                </span>
                {"\n"}
                <span className="text-layer-violet">strata</span>.map(
                <span className="text-layer-teal">&quot;g p&quot;</span>,{" "}
                <span className="text-layer-violet">function</span>(ctx){"\n"}
                {"  "}
                <span className="text-layer-violet">strata</span>.cd(os.getenv(
                <span className="text-layer-teal">&quot;HOME&quot;</span>) ..{" "}
                <span className="text-layer-teal">&quot;/projects&quot;</span>)
                {"\n"}
                <span className="text-layer-violet">end</span>,{" "}
                <span className="text-layer-teal">
                  &quot;Go to projects&quot;
                </span>
                ){"\n\n"}
                <span className="text-layer-violet">strata</span>.statusline(
                <span className="text-layer-violet">function</span>(ctx){"\n"}
                {"  "}
                <span className="text-layer-violet">return</span> #ctx.selected
                ..{" "}
                <span className="text-layer-teal">&quot; selected&quot;</span>
                {"\n"}
                <span className="text-layer-violet">end</span>)
              </code>
            </pre>
            <ul className="space-y-5 text-mist-300">
              {[
                [
                  "Keys and commands",
                  "Bind any key sequence or add :commands to the palette.",
                ],
                [
                  "UI panels and previewers",
                  "Render your own panel next to the files, or preview new formats.",
                ],
                [
                  "Safe by design",
                  "A broken plugin shows an error — it never takes strata down.",
                ],
              ].map(([title, text]) => (
                <li key={title} className="flex gap-4">
                  <span className="mt-1.5 size-2 shrink-0 rounded-full bg-gradient-to-br from-layer-violet to-layer-sky" />
                  <span>
                    <span className="font-semibold text-white">{title}.</span>{" "}
                    {text}
                  </span>
                </li>
              ))}
              <li>
                <Link
                  href="/docs/plugins/"
                  className="inline-flex items-center gap-2 font-semibold text-layer-sky hover:underline"
                >
                  Read the plugin API <ArrowRight className="size-4" />
                </Link>
              </li>
            </ul>
          </div>
        </Section>

        {/* CTA */}
        <section className="mx-auto max-w-7xl px-5 pb-28">
          <div className="relative overflow-hidden rounded-3xl border border-white/[0.07] bg-ink-850 px-8 py-16 text-center">
            <div
              aria-hidden
              className="absolute inset-0 -z-0 bg-gradient-to-br from-layer-violet/10 via-transparent to-layer-teal/10"
            />
            <h2 className="relative text-3xl font-semibold tracking-tight text-white sm:text-4xl">
              Install strata in under a minute.
            </h2>
            <p className="relative mx-auto mt-4 max-w-xl text-mist-300">
              One command with Cargo, or grab a prebuilt binary for Linux, macOS
              or Windows from the releases page.
            </p>
            <div className="relative mt-8 flex justify-center">
              <CopyCommand command={site.install} />
            </div>
          </div>
        </section>
      </main>
      <SiteFooter />
    </>
  );
}
