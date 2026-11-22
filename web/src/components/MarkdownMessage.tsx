"use client";

import React, { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Copy } from "lucide-react";
import { Button } from "./ui/button";

interface MarkdownMessageProps {
  content: string;
}

function safeUrlTransform(url: string): string {
  const trimmed = url.trim();
  if (/^(https?:|mailto:|#)/i.test(trimmed)) {
    return trimmed;
  }
  return "";
}

export function MarkdownMessage({ content }: MarkdownMessageProps) {
  return (
    <div className="max-w-none space-y-2 break-words leading-relaxed">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        disallowedElements={["img"]}
        urlTransform={safeUrlTransform}
        components={{
          pre({ children }) {
            return <>{children}</>;
          },
          a({ href, children }) {
            return (
              <a
                href={href}
                target="_blank"
                rel="noopener noreferrer"
                className="text-primary underline underline-offset-2 hover:opacity-80"
              >
                {children}
              </a>
            );
          },
          code({ node: _node, className, children, ...props }) {
            void _node;
            const match = /language-([^\s]+)/.exec(className || "");
            const text = String(children).replace(/\n$/, "");
            const isInline = !match && !text.includes("\n");

            if (isInline) {
              return (
                <code
                  className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs font-semibold"
                  {...props}
                >
                  {children}
                </code>
              );
            }

            return <CodeBlock language={match?.[1] || "text"} value={text} />;
          },
          table({ children }) {
            return (
              <div className="my-3 overflow-x-auto rounded-md border">
                <table className="w-full text-left text-xs">{children}</table>
              </div>
            );
          },
          th({ children }) {
            return <th className="border-b bg-muted/60 p-2 font-medium">{children}</th>;
          },
          td({ children }) {
            return <td className="border-b p-2 last:border-b-0">{children}</td>;
          },
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}

function CodeBlock({ language, value }: { language: string; value: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // fallback
    }
  };

  return (
    <div className="group relative my-3 overflow-hidden rounded-lg border bg-zinc-950 text-zinc-100 dark:bg-zinc-900">
      <div className="flex items-center justify-between border-b border-zinc-800 bg-zinc-900/80 px-3 py-1.5 text-xs text-zinc-400">
        <span className="font-mono uppercase">{language}</span>
        <Button
          variant="ghost"
          size="sm"
          onClick={handleCopy}
          className="h-6 gap-1 px-2 text-xs text-zinc-400 hover:bg-zinc-800 hover:text-zinc-100"
        >
          {copied ? (
            <>
              <Check className="h-3 w-3 text-emerald-400" />
              <span>Copied</span>
            </>
          ) : (
            <>
              <Copy className="h-3 w-3" />
              <span>Copy</span>
            </>
          )}
        </Button>
      </div>
      <div className="overflow-x-auto p-3 font-mono text-xs leading-5">
        <pre>{value}</pre>
      </div>
    </div>
  );
}
