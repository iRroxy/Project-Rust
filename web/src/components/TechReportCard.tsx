"use client";

import React from "react";
import type { TechReport } from "@/types/api";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { CheckCircle2, XCircle, Gauge } from "lucide-react";

interface TechReportCardProps {
  report: TechReport;
}

export function TechReportCard({ report }: TechReportCardProps) {
  const getScoreColor = (score: number) => {
    if (score >= 80) return "text-emerald-500 bg-emerald-500/10 border-emerald-500/20";
    if (score >= 60) return "text-amber-500 bg-amber-500/10 border-amber-500/20";
    return "text-rose-500 bg-rose-500/10 border-rose-500/20";
  };

  return (
    <Card className="my-3 border-border/80 shadow-sm transition-all hover:shadow-md">
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <Gauge className="h-4 w-4 text-primary" />
            <CardTitle className="text-sm font-semibold tracking-tight">
              技术深度分析报告
            </CardTitle>
          </div>
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground">综合评分</span>
            <Badge
              variant="outline"
              className={`font-mono text-xs font-bold ${getScoreColor(report.score)}`}
            >
              {report.score} / 100
            </Badge>
          </div>
        </div>
      </CardHeader>

      <CardContent className="space-y-4 text-xs">
        {/* Score Progress Bar */}
        <div className="space-y-1">
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-secondary">
            <div
              className={`h-full transition-all duration-500 ${
                report.score >= 80
                  ? "bg-emerald-500"
                  : report.score >= 60
                  ? "bg-amber-500"
                  : "bg-rose-500"
              }`}
              style={{ width: `${Math.min(100, Math.max(0, report.score))}%` }}
            />
          </div>
        </div>

        {/* Summary */}
        <div className="rounded-lg bg-muted/50 p-2.5 text-muted-foreground">
          <p className="font-medium text-foreground">{report.summary}</p>
        </div>

        {/* Pros & Cons Grid */}
        <div className="grid gap-3 sm:grid-cols-2">
          {/* Pros */}
          <div className="space-y-2 rounded-lg border border-emerald-500/20 bg-emerald-500/5 p-2.5">
            <div className="flex items-center gap-1.5 font-medium text-emerald-600 dark:text-emerald-400">
              <CheckCircle2 className="h-3.5 w-3.5" />
              <span>优势 (Pros)</span>
            </div>
            <ul className="space-y-1 text-muted-foreground">
              {report.pros.map((pro, i) => (
                <li key={i} className="flex items-start gap-1.5">
                  <span className="text-emerald-500">•</span>
                  <span>{pro}</span>
                </li>
              ))}
            </ul>
          </div>

          {/* Cons */}
          <div className="space-y-2 rounded-lg border border-rose-500/20 bg-rose-500/5 p-2.5">
            <div className="flex items-center gap-1.5 font-medium text-rose-600 dark:text-rose-400">
              <XCircle className="h-3.5 w-3.5" />
              <span>考量 (Cons)</span>
            </div>
            <ul className="space-y-1 text-muted-foreground">
              {report.cons.map((con, i) => (
                <li key={i} className="flex items-start gap-1.5">
                  <span className="text-rose-500">•</span>
                  <span>{con}</span>
                </li>
              ))}
            </ul>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}
