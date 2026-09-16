import { open } from "node:fs/promises";

export type BoundedReadResult = {
  buffer: Buffer;
  truncated: boolean;
};

export async function readFilePrefix(absolutePath: string, maxBytes: number): Promise<BoundedReadResult> {
  const handle = await open(absolutePath, "r");
  try {
    const buffer = Buffer.alloc(maxBytes + 1);
    const { bytesRead } = await handle.read(buffer, 0, buffer.length, 0);
    return {
      buffer: buffer.subarray(0, Math.min(bytesRead, maxBytes)),
      truncated: bytesRead > maxBytes
    };
  } finally {
    await handle.close();
  }
}
