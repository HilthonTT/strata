export type DocLink = { title: string; href: string };
export type DocSection = { title: string; links: DocLink[] };

/** Order of the docs sidebar and the previous/next links. */
export const docs: DocSection[] = [
  {
    title: "Getting started",
    links: [
      { title: "Introduction", href: "/docs/" },
      { title: "Installation", href: "/docs/installation/" },
      { title: "Quick start", href: "/docs/quick-start/" },
    ],
  },
  {
    title: "Using strata",
    links: [
      { title: "Panels & tabs", href: "/docs/panels-and-tabs/" },
      { title: "File operations", href: "/docs/file-operations/" },
      { title: "Search", href: "/docs/search/" },
      { title: "Preview & metadata", href: "/docs/preview/" },
      { title: "Git status", href: "/docs/git/" },
    ],
  },
  {
    title: "Views",
    links: [
      { title: "Dashboard", href: "/docs/dashboard/" },
      { title: "Docker", href: "/docs/docker/" },
      { title: "NAS connections", href: "/docs/nas/" },
    ],
  },
  {
    title: "Customize",
    links: [
      { title: "Configuration", href: "/docs/configuration/" },
      { title: "Key bindings", href: "/docs/keybindings/" },
      { title: "Themes", href: "/docs/themes/" },
      { title: "Plugins", href: "/docs/plugins/" },
      { title: "Shell integration", href: "/docs/shell-integration/" },
    ],
  },
  {
    title: "Help",
    links: [{ title: "Troubleshooting", href: "/docs/troubleshooting/" }],
  },
];

export const allDocs: DocLink[] = docs.flatMap((s) => s.links);
