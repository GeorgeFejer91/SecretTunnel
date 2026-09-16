import { z } from "zod";
import { RepoInputSchema } from "./repo.contract.js";

export const SearchInputSchema = RepoInputSchema.extend({
  query: z.string().min(1),
  mode: z.enum(["literal", "regex"]).default("literal"),
  include_globs: z.array(z.string()).optional(),
  exclude_globs: z.array(z.string()).optional(),
  context_lines: z.number().int().min(0).max(5).optional(),
  max_results: z.number().int().positive().optional(),
  cursor: z.string().optional()
});

export const SearchResultSchema = z.object({
  path: z.string(),
  line: z.number().int().positive(),
  column: z.number().int().positive(),
  text: z.string(),
  before: z.array(z.string()).default([]),
  after: z.array(z.string()).default([])
});

export const SearchResponseSchema = z.object({
  results: z.array(SearchResultSchema),
  matched_count: z.number().int().nonnegative(),
  returned_count: z.number().int().nonnegative(),
  truncated: z.boolean(),
  next_cursor: z.string().optional(),
  warnings: z.array(z.string()).default([])
});
