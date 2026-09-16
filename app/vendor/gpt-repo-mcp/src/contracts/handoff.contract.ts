import { z } from "zod";
import { RepoInputSchema } from "./repo.contract.js";

const NonEmptyStringSchema = z.string().min(1);

export const HandoffNextStepSchema = z.object({
  title: NonEmptyStringSchema.describe("Short title for the next concrete step."),
  goal: z.string().min(1).optional().describe("The intended outcome for this step."),
  done_when: z.string().min(1).optional().describe("Concrete completion condition for this step.")
});

export const HandoffInputSchema = RepoInputSchema.extend({
  title: NonEmptyStringSchema.describe("Human-readable title for this handoff. Used to generate the local markdown slug."),
  current_track: z.string().min(1).optional().describe("Optional workstream, slice, or branch of effort currently active."),
  current_state: NonEmptyStringSchema.describe("What is true right now and where the session is stopping."),
  why: NonEmptyStringSchema.describe("Why this handoff exists and what future context matters."),
  completed_work: z.array(z.string().min(1)).optional().describe("Work completed in this session."),
  decisions: z.array(z.string().min(1)).optional().describe("Decisions made that the next session should preserve."),
  workflow: z.array(z.string().min(1)).optional().describe("Relevant workflow or process notes."),
  constraints: z.array(z.string().min(1)).optional().describe("Constraints, safety rules, or boundaries to preserve."),
  next_steps: z.array(HandoffNextStepSchema).min(1).describe("Ordered concrete next steps. At least one item is required."),
  important_files: z.array(z.string().min(1)).optional().describe("Repo-relative files that matter for resume context."),
  risks: z.array(z.string().min(1)).optional().describe("Known risks or caveats."),
  open_questions: z.array(z.string().min(1)).optional().describe("Open questions for a later session."),
  update_current: z.boolean().optional().describe("When false, only the detailed handoff is written. Defaults to true."),
  dry_run: z.boolean().optional().describe("Validate and render without writing any files.")
});

export const HandoffResultSchema = z.object({
  ok: z.literal(true).describe("True when the handoff write completed or dry-run validation succeeded."),
  dry_run: z.boolean().describe("Whether the request was validation-only and did not write files."),
  handoff_path: z.string().describe("Repo-relative detailed handoff path under .chatgpt/handoffs ending in .local.md."),
  current_path: z.string().optional().describe("Repo-relative current handoff pointer path when update_current is enabled."),
  updated_current: z.boolean().describe("Whether current.local.md was written or dry-run validated."),
  branch: z.string().describe("Current git branch from repo_git_status semantics."),
  head_sha: z.string().describe("Current HEAD SHA from git status."),
  clean: z.boolean().describe("Whether git status was clean before writing the handoff."),
  startup_prompt: z.string().describe("Prompt fragment for resuming from the local handoff."),
  current_next_step: z.string().describe("Title of the first requested next step."),
  warnings: z.array(z.string()).describe("Non-fatal warnings from rendering or write validation.")
});

export type HandoffNextStep = z.infer<typeof HandoffNextStepSchema>;
export type HandoffInput = z.infer<typeof HandoffInputSchema>;
export type HandoffResult = z.infer<typeof HandoffResultSchema>;
