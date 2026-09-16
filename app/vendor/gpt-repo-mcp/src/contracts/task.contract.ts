import { z } from "zod";
import { GlobScopeSchema } from "./file.contract.js";
import { RepoInputSchema } from "./repo.contract.js";

export const TaskKindSchema = z.enum(["todo", "fixme", "hack", "checkbox", "roadmap"]);

export const TaskInventoryInputSchema = RepoInputSchema
  .merge(GlobScopeSchema)
  .extend({
    labels: z.array(TaskKindSchema).optional(),
    max_results: z.number().int().positive().optional(),
    cursor: z.string().optional()
  });

export const TaskInventoryItemSchema = z.object({
  path: z.string(),
  line: z.number().int().positive(),
  kind: TaskKindSchema,
  text: z.string(),
  surrounding_context: z.string().optional()
});

export const TaskInventoryResultSchema = z.object({
  tasks: z.array(TaskInventoryItemSchema),
  matched_count: z.number().int().nonnegative(),
  returned_count: z.number().int().nonnegative(),
  scanned_file_count: z.number().int().nonnegative(),
  scan_complete: z.boolean(),
  truncated: z.boolean(),
  next_cursor: z.string().optional(),
  warnings: z.array(z.string()).default([])
});

export type TaskInventoryInput = z.infer<typeof TaskInventoryInputSchema>;
export type TaskKind = z.infer<typeof TaskKindSchema>;
