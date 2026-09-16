import { z } from "zod";
import { RepoInputSchema } from "./repo.contract.js";

export const CleanupPathsInputSchema = RepoInputSchema.extend({
  paths: z.array(z.string().min(1)).min(1).describe("Explicit repo-relative POSIX files or directories to clean up. Only paths matching cleanup policy and not tracked by Git are eligible."),
  dry_run: z.boolean().optional().describe("Preview which explicit paths would be deleted without removing files or directories."),
  reason: z.string().min(1).optional().describe("Short human-readable reason for the cleanup request, useful for audit context.")
});

export const CleanupPathsResultSchema = z.object({
  ok: z.literal(true).describe("True when cleanup completed or dry-run validation succeeded."),
  dry_run: z.boolean().describe("Whether the request was validation-only and did not delete anything."),
  deleted: z.array(z.object({
    path: z.string().describe("Repo-relative path deleted or that would be deleted during dry-run."),
    type: z.enum(["file", "directory"]).describe("Kind of filesystem entry deleted or that would be deleted.")
  })).describe("Explicit repo-relative files or directories deleted, or previewed during dry-run."),
  skipped: z.array(z.object({
    path: z.string().describe("Repo-relative path that was not deleted."),
    reason: z.string().describe("Stable reason explaining why the path was skipped.")
  })).describe("Explicit paths that were not deleted and the reason for each skip."),
  warnings: z.array(z.string()).describe("Non-fatal warnings produced by the cleanup service.")
});

export type CleanupPathsInput = z.infer<typeof CleanupPathsInputSchema>;
export type CleanupPathsResult = z.infer<typeof CleanupPathsResultSchema>;
