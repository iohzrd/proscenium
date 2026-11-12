import privacySource from "../../../docs/privacy-policy.md?raw";
import termsSource from "../../../docs/terms-of-use.md?raw";

export type LegalBlock =
  | { kind: "title"; text: string }
  | { kind: "heading"; text: string }
  | { kind: "paragraph"; text: string }
  | { kind: "list"; items: string[] };

export type LegalDocumentId = "terms" | "privacy";

/**
 * Parse the Markdown subset used in `docs/terms-of-use.md` and
 * `docs/privacy-policy.md`: `# ` title, `## ` headings, paragraphs and `- `
 * list items. A line indented below a list item continues the item.
 */
export function parseLegalMarkdown(source: string): LegalBlock[] {
  const blocks: LegalBlock[] = [];
  let paragraph: string[] = [];
  let list: string[] | null = null;
  const flush = () => {
    if (paragraph.length > 0) {
      blocks.push({ kind: "paragraph", text: paragraph.join(" ") });
      paragraph = [];
    }
    if (list) {
      blocks.push({ kind: "list", items: list });
      list = null;
    }
  };
  for (const raw of source.split("\n")) {
    const line = raw.trim();
    if (!line) {
      flush();
    } else if (line.startsWith("# ")) {
      flush();
      blocks.push({ kind: "title", text: line.slice(2) });
    } else if (line.startsWith("## ")) {
      flush();
      blocks.push({ kind: "heading", text: line.slice(3) });
    } else if (line.startsWith("- ")) {
      if (paragraph.length > 0) flush();
      (list ??= []).push(line.slice(2));
    } else if (list && /^\s/.test(raw)) {
      list[list.length - 1] += ` ${line}`;
    } else {
      if (list) flush();
      paragraph.push(line);
    }
  }
  flush();
  return blocks;
}

export const LEGAL_DOCUMENTS: Record<LegalDocumentId, LegalBlock[]> = {
  terms: parseLegalMarkdown(termsSource),
  privacy: parseLegalMarkdown(privacySource),
};
