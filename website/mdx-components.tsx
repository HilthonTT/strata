import type { MDXComponents } from "mdx/types";

import { Media } from "@/components/media";
import { Callout, Kbd } from "@/components/mdx-parts";

export function useMDXComponents(components: MDXComponents): MDXComponents {
  return { ...components, Media, Callout, Kbd };
}
