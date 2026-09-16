import { z } from "zod";
import { RepoInputSchema } from "./repo.contract.js";

export const DecisionSourceSchema = z.enum(["docs", "readme", "agents", "comments", "package"]);
export const ConfidenceSchema = z.enum(["low", "medium", "high"]);

export const DecisionLogInputSchema = RepoInputSchema.extend({
  include_sources: z.array(DecisionSourceSchema).optional()
});

export const DecisionEvidenceSchema = z.object({
  path: z.string(),
  line: z.number().int().positive().optional(),
  quote: z.string().optional(),
  source_type: DecisionSourceSchema.optional()
});

export const DecisionLogResultSchema = z.object({
  decisions: z.array(z.object({
    title: z.string(),
    decision: z.string(),
    evidence: z.array(DecisionEvidenceSchema),
    confidence: ConfidenceSchema
  })),
  conventions: z.array(z.object({
    area: z.string(),
    rule: z.string(),
    evidence: z.array(z.object({
      path: z.string(),
      line: z.number().int().positive().optional()
    }))
  })),
  gaps: z.array(z.string()),
  warnings: z.array(z.string())
});

export type DecisionLogInput = z.infer<typeof DecisionLogInputSchema>;
export type DecisionSource = z.infer<typeof DecisionSourceSchema>;
