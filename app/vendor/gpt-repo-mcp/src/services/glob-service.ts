export function matchesGlob(path: string, glob: string): boolean {
  return globToRegExp(glob).test(path);
}

export function isExcludedByGlob(path: string, excludeGlobs: string[] = []): boolean {
  return excludeGlobs.some((glob) => matchesGlob(path, glob));
}

function globToRegExp(glob: string): RegExp {
  let pattern = "";
  for (let index = 0; index < glob.length; index += 1) {
    const char = glob[index];
    const next = glob[index + 1];

    if (char === "*" && next === "*") {
      const afterNext = glob[index + 2];
      if (afterNext === "/") {
        pattern += "(?:.*/)?";
        index += 2;
      } else {
        pattern += ".*";
        index += 1;
      }
      continue;
    }
    if (char === "*") {
      pattern += "[^/]*";
      continue;
    }
    pattern += escapeRegExp(char);
  }
  return new RegExp(`^${pattern}$`);
}

function escapeRegExp(value: string): string {
  return value.replace(/[|\\{}()[\]^$+?.]/g, "\\$&");
}
