// A minimal Markdown renderer for previews, ported from the Mac app: split
// into blocks (headings, paragraphs, list items, code fences, rules), then
// style inline bold, italics, code, and links. Output is React elements,
// never raw HTML, so a previewed file can't inject markup.

import { Fragment, type ReactNode } from "react";

import { api } from "../lib/api";

type Block =
  | { type: "heading"; level: number; text: string }
  | { type: "paragraph"; text: string }
  | { type: "listItem"; text: string }
  | { type: "code"; text: string }
  | { type: "rule" };

export function parseMarkdown(source: string): Block[] {
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  let code: string[] | null = null;

  const flush = () => {
    const text = paragraph.join(" ").trim();
    if (text) blocks.push({ type: "paragraph", text });
    paragraph = [];
  };

  for (const raw of source.split(/\r?\n/)) {
    const trimmed = raw.trim();
    if (code) {
      if (trimmed.startsWith("```")) {
        blocks.push({ type: "code", text: code.join("\n") });
        code = null;
      } else {
        code.push(raw);
      }
      continue;
    }
    if (trimmed.startsWith("```")) {
      flush();
      code = [];
      continue;
    }
    if (!trimmed) {
      flush();
      continue;
    }
    if (trimmed === "---" || trimmed === "***" || trimmed === "___") {
      flush();
      blocks.push({ type: "rule" });
      continue;
    }
    const heading = /^(#{1,6}) (.*)$/.exec(trimmed);
    if (heading) {
      flush();
      blocks.push({ type: "heading", level: heading[1].length, text: heading[2].trim() });
      continue;
    }
    if (/^[-*+] /.test(trimmed)) {
      flush();
      blocks.push({ type: "listItem", text: trimmed.slice(2) });
      continue;
    }
    paragraph.push(trimmed);
  }
  flush();
  if (code) blocks.push({ type: "code", text: code.join("\n") });
  return blocks;
}

const inlinePattern = /(`[^`]+`)|(\*\*[^*]+\*\*|__[^_]+__)|(\*[^*\s][^*]*\*|_[^_\s][^_]*_)|(\[[^\]]+\]\([^)\s]+\))/;

function inline(text: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let rest = text;
  let key = 0;
  while (rest) {
    const match = inlinePattern.exec(rest);
    if (!match) {
      nodes.push(rest);
      break;
    }
    if (match.index > 0) nodes.push(rest.slice(0, match.index));
    const token = match[0];
    if (match[1]) {
      nodes.push(<code key={key++}>{token.slice(1, -1)}</code>);
    } else if (match[2]) {
      nodes.push(<strong key={key++}>{inline(token.slice(2, -2))}</strong>);
    } else if (match[3]) {
      nodes.push(<em key={key++}>{inline(token.slice(1, -1))}</em>);
    } else {
      const [, label, href] = /^\[([^\]]+)\]\(([^)\s]+)\)$/.exec(token) ?? [];
      const external = /^https?:\/\//.test(href ?? "");
      nodes.push(
        <a
          key={key++}
          href={external ? href : undefined}
          onClick={(event) => {
            event.preventDefault();
            if (external) api.openUrl(href);
          }}
        >
          {inline(label ?? token)}
        </a>,
      );
    }
    rest = rest.slice(match.index + token.length);
  }
  return nodes;
}

export function Markdown({ source }: { source: string }) {
  return (
    <div className="markdown">
      {parseMarkdown(source).map((block, index) => {
        switch (block.type) {
          case "heading": {
            const Tag = `h${Math.min(block.level, 4)}` as "h1";
            return <Tag key={index}>{inline(block.text)}</Tag>;
          }
          case "paragraph":
            return <p key={index}>{inline(block.text)}</p>;
          case "listItem":
            return (
              <div key={index} className="markdown-li">
                <span>•</span>
                <span>{inline(block.text)}</span>
              </div>
            );
          case "code":
            return <pre key={index}>{block.text}</pre>;
          case "rule":
            return <hr key={index} />;
          default:
            return <Fragment key={index} />;
        }
      })}
    </div>
  );
}
