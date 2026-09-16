import { extname } from "node:path";
import { IgnoreEngine } from "./ignore-engine.js";
import { readFilePrefix } from "./bounded-read.js";

const LANGUAGE_BY_EXT: Record<string, string> = {
  ".ts": "typescript",
  ".tsx": "tsx",
  ".js": "javascript",
  ".jsx": "jsx",
  ".json": "json",
  ".md": "markdown",
  ".py": "python",
  ".go": "go",
  ".rs": "rust",
  ".java": "java",
  ".css": "css",
  ".html": "html",
  ".yml": "yaml",
  ".yaml": "yaml"
};

export type FileClassification = {
  path: string;
  language?: string;
  is_binary: boolean;
  is_secret_candidate: boolean;
  is_generated: boolean;
};

export class FileClassifier {
  constructor(private readonly ignoreEngine = new IgnoreEngine()) {}

  async classify(repoPath: string, absolutePath: string): Promise<FileClassification> {
    return {
      path: repoPath,
      language: LANGUAGE_BY_EXT[extname(repoPath).toLowerCase()],
      is_binary: await isBinaryFile(absolutePath),
      is_secret_candidate: this.ignoreEngine.isSensitiveCandidate(repoPath),
      is_generated: this.ignoreEngine.isIgnored(repoPath)
    };
  }
}

async function isBinaryFile(absolutePath: string): Promise<boolean> {
  const { buffer } = await readFilePrefix(absolutePath, 4096);
  return buffer.includes(0);
}
