const OWNER = "1tuz";
const REPO = "impetus";
const BASE_BRANCH = "main";

const MAX_STAGE_LENGTH = 6000;
const MAX_FILES = 100;
const MAX_CHUNKS_PER_FILE = 512;
const MAX_MANIFEST_CHUNKS = 64;

type ManifestFile = {
  path: string;
  chunks: string[];
  mode?: "100644" | "100755";
};

type Manifest = {
  branch: string;
  message: string;
  title?: string;
  body?: string;
  draft?: boolean;
  openPr?: boolean;
  files?: ManifestFile[];
  delete?: string[];
};

class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

function json(data: unknown, status = 200) {
  return new Response(JSON.stringify(data, null, 2), {
    status,
    headers: {
      "content-type": "application/json; charset=utf-8",
      "cache-control": "no-store",
    },
  });
}

function constantTimeEqual(a: string, b: string) {
  const left = new TextEncoder().encode(a);
  const right = new TextEncoder().encode(b);

  if (left.length !== right.length) return false;

  let diff = 0;
  for (let i = 0; i < left.length; i++) {
    diff |= left[i] ^ right[i];
  }

  return diff === 0;
}

function authorized(url: URL) {
  const expected = Deno.env.get("BRIDGE_SECRET");
  const supplied = url.searchParams.get("key");

  return Boolean(
    expected &&
      supplied &&
      constantTimeEqual(expected, supplied),
  );
}

function githubToken() {
  const token = Deno.env.get("GITHUB_TOKEN");
  if (!token) throw new ApiError(500, "GITHUB_TOKEN is not configured");
  return token;
}

async function github(path: string, init: RequestInit = {}) {
  return await fetch(`https://api.github.com${path}`, {
    ...init,
    headers: {
      Authorization: `Bearer ${githubToken()}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": "impetus-bridge",
      "content-type": "application/json",
      ...(init.headers ?? {}),
    },
  });
}

async function githubJson(
  path: string,
  init: RequestInit = {},
): Promise<any> {
  const response = await github(path, init);
  const text = await response.text();

  let data: any = null;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = { message: text };
    }
  }

  if (!response.ok) {
    throw new ApiError(
      502,
      `GitHub ${response.status}: ${data?.message ?? "request failed"}`,
    );
  }

  return data;
}

function encodeRef(ref: string) {
  return ref
    .split("/")
    .map(encodeURIComponent)
    .join("/");
}

function validateSha(sha: string) {
  if (!/^[0-9a-f]{40}$/i.test(sha)) {
    throw new ApiError(400, "Invalid blob SHA");
  }
}

function validateBranch(branch: string) {
  if (!/^fix\/chatgpt-[a-z0-9._-]{1,80}$/.test(branch)) {
    throw new ApiError(
      400,
      "Branch must match fix/chatgpt-*",
    );
  }
}

function validatePath(path: string) {
  if (
    !path ||
    path.length > 300 ||
    path.startsWith("/") ||
    path.includes("\\") ||
    path.split("/").some((part) =>
      part === "" || part === "." || part === ".."
    )
  ) {
    throw new ApiError(400, `Invalid path: ${path}`);
  }

  // Bridge не может переписывать сам себя или workflows.
  if (
    path === "bridge.ts" ||
    path.startsWith(".github/workflows/") ||
    path === ".env" ||
    path.startsWith(".env.") ||
    path.startsWith(".git/")
  ) {
    throw new ApiError(403, `Protected path: ${path}`);
  }
}

function base64UrlToBase64(value: string) {
  const normalized = value
    .replace(/-/g, "+")
    .replace(/_/g, "/");

  return normalized + "=".repeat(
    (4 - (normalized.length % 4)) % 4,
  );
}

function base64ToBytes(value: string) {
  const clean = value.replace(/\s/g, "");
  const binary = atob(clean);

  const result = new Uint8Array(binary.length);

  for (let i = 0; i < binary.length; i++) {
    result[i] = binary.charCodeAt(i);
  }

  return result;
}

function bytesToBase64(bytes: Uint8Array) {
  let binary = "";
  const step = 0x8000;

  for (let i = 0; i < bytes.length; i += step) {
    binary += String.fromCharCode(
      ...bytes.subarray(i, i + step),
    );
  }

  return btoa(binary);
}

function concatBytes(parts: Uint8Array[]) {
  const length = parts.reduce(
    (sum, part) => sum + part.length,
    0,
  );

  const output = new Uint8Array(length);

  let offset = 0;
  for (const part of parts) {
    output.set(part, offset);
    offset += part.length;
  }

  return output;
}

async function readBlob(sha: string) {
  validateSha(sha);

  const blob = await githubJson(
    `/repos/${OWNER}/${REPO}/git/blobs/${sha}`,
  );

  if (blob.encoding !== "base64") {
    throw new ApiError(502, "Unexpected GitHub blob encoding");
  }

  return base64ToBytes(blob.content);
}

async function readBlobSequence(shas: string[]) {
  const parts: Uint8Array[] = [];

  for (const sha of shas) {
    parts.push(await readBlob(sha));
  }

  return concatBytes(parts);
}

async function getBranchHead(branch: string) {
  const response = await github(
    `/repos/${OWNER}/${REPO}/git/ref/heads/${encodeRef(branch)}`,
  );

  if (response.status === 404) {
    return null;
  }

  if (!response.ok) {
    throw new ApiError(
      502,
      `Cannot read branch ${branch}: GitHub ${response.status}`,
    );
  }

  return await response.json();
}

async function cleanupExistingPr(branch: string) {
  const params = new URLSearchParams({
    state: "open",
    head: `${OWNER}:${branch}`,
    base: BASE_BRANCH,
  });

  const prs = await githubJson(
    `/repos/${OWNER}/${REPO}/pulls?${params}`,
  );

  return Array.isArray(prs) && prs.length > 0
    ? prs[0]
    : null;
}

function validateManifest(input: any): Manifest {
  if (!input || typeof input !== "object") {
    throw new ApiError(400, "Invalid manifest");
  }

  if (typeof input.branch !== "string") {
    throw new ApiError(400, "Manifest branch is required");
  }

  validateBranch(input.branch);

  if (
    typeof input.message !== "string" ||
    !input.message.trim() ||
    input.message.length > 300
  ) {
    throw new ApiError(400, "Invalid commit message");
  }

  const files: ManifestFile[] = input.files ?? [];
  const deletions: string[] = input.delete ?? [];

  if (!Array.isArray(files) || !Array.isArray(deletions)) {
    throw new ApiError(400, "Invalid files/delete manifest");
  }

  if (files.length > MAX_FILES) {
    throw new ApiError(400, "Too many files");
  }

  if (files.length === 0 && deletions.length === 0) {
    throw new ApiError(400, "Manifest contains no changes");
  }

  const paths = new Set<string>();

  for (const file of files) {
    if (
      !file ||
      typeof file.path !== "string" ||
      !Array.isArray(file.chunks)
    ) {
      throw new ApiError(400, "Invalid file entry");
    }

    validatePath(file.path);

    if (paths.has(file.path)) {
      throw new ApiError(400, `Duplicate path: ${file.path}`);
    }

    paths.add(file.path);

    if (file.chunks.length > MAX_CHUNKS_PER_FILE) {
      throw new ApiError(
        400,
        `Too many chunks for ${file.path}`,
      );
    }

    for (const sha of file.chunks) {
      validateSha(sha);
    }

    if (
      file.mode !== undefined &&
      file.mode !== "100644" &&
      file.mode !== "100755"
    ) {
      throw new ApiError(400, `Invalid mode for ${file.path}`);
    }
  }

  for (const path of deletions) {
    if (typeof path !== "string") {
      throw new ApiError(400, "Invalid delete path");
    }

    validatePath(path);

    if (paths.has(path)) {
      throw new ApiError(
        400,
        `Path cannot be written and deleted: ${path}`,
      );
    }

    paths.add(path);
  }

  return {
    branch: input.branch,
    message: input.message.trim(),
    title: typeof input.title === "string"
      ? input.title.slice(0, 200)
      : undefined,
    body: typeof input.body === "string"
      ? input.body.slice(0, 20_000)
      : undefined,
    draft: input.draft !== false,
    openPr: input.openPr !== false,
    files,
    delete: deletions,
  };
}

async function stage(url: URL) {
  const data = url.searchParams.get("data");

  if (data === null) {
    throw new ApiError(400, "Missing data");
  }

  if (data.length > MAX_STAGE_LENGTH) {
    throw new ApiError(
      413,
      `Chunk too large; max encoded length is ${MAX_STAGE_LENGTH}`,
    );
  }

  if (!/^[A-Za-z0-9_-]*$/.test(data)) {
    throw new ApiError(400, "Chunk must be base64url");
  }

  const blob = await githubJson(
    `/repos/${OWNER}/${REPO}/git/blobs`,
    {
      method: "POST",
      body: JSON.stringify({
        content: base64UrlToBase64(data),
        encoding: "base64",
      }),
    },
  );

  return json({
    ok: true,
    sha: blob.sha,
  });
}

async function apply(url: URL) {
  const rawManifest = url.searchParams.get("manifest");

  if (!rawManifest) {
    throw new ApiError(400, "Missing manifest blob SHA(s)");
  }

  const manifestShas = rawManifest
    .split(",")
    .map((sha) => sha.trim())
    .filter(Boolean);

  if (
    manifestShas.length === 0 ||
    manifestShas.length > MAX_MANIFEST_CHUNKS
  ) {
    throw new ApiError(400, "Invalid manifest blob list");
  }

  for (const sha of manifestShas) validateSha(sha);

  const manifestBytes = await readBlobSequence(manifestShas);

  let decodedManifest: unknown;

  try {
    decodedManifest = JSON.parse(
      new TextDecoder().decode(manifestBytes),
    );
  } catch {
    throw new ApiError(400, "Manifest JSON is invalid");
  }

  const manifest = validateManifest(decodedManifest);

  // Если fix-ветка уже существует — продолжаем её.
  // Иначе начинаем от текущего main.
  const existingBranch = await getBranchHead(manifest.branch);

  const parentSha = existingBranch
    ? existingBranch.object.sha
    : (await githubJson(
      `/repos/${OWNER}/${REPO}/git/ref/heads/${BASE_BRANCH}`,
    )).object.sha;

  const parentCommit = await githubJson(
    `/repos/${OWNER}/${REPO}/git/commits/${parentSha}`,
  );

  const tree: any[] = [];

  for (const file of manifest.files ?? []) {
    let blobSha: string;

    if (file.chunks.length === 0) {
      const emptyBlob = await githubJson(
        `/repos/${OWNER}/${REPO}/git/blobs`,
        {
          method: "POST",
          body: JSON.stringify({
            content: "",
            encoding: "utf-8",
          }),
        },
      );

      blobSha = emptyBlob.sha;
    } else if (file.chunks.length === 1) {
      // Один staged blob уже является готовым содержимым файла.
      blobSha = file.chunks[0];
    } else {
      const contents = await readBlobSequence(file.chunks);

      const blob = await githubJson(
        `/repos/${OWNER}/${REPO}/git/blobs`,
        {
          method: "POST",
          body: JSON.stringify({
            content: bytesToBase64(contents),
            encoding: "base64",
          }),
        },
      );

      blobSha = blob.sha;
    }

    tree.push({
      path: file.path,
      mode: file.mode ?? "100644",
      type: "blob",
      sha: blobSha,
    });
  }

  for (const path of manifest.delete ?? []) {
    tree.push({
      path,
      mode: "100644",
      type: "blob",
      sha: null,
    });
  }

  const newTree = await githubJson(
    `/repos/${OWNER}/${REPO}/git/trees`,
    {
      method: "POST",
      body: JSON.stringify({
        base_tree: parentCommit.tree.sha,
        tree,
      }),
    },
  );

  const commit = await githubJson(
    `/repos/${OWNER}/${REPO}/git/commits`,
    {
      method: "POST",
      body: JSON.stringify({
        message: manifest.message,
        tree: newTree.sha,
        parents: [parentSha],
      }),
    },
  );

  if (existingBranch) {
    await githubJson(
      `/repos/${OWNER}/${REPO}/git/refs/heads/${
        encodeRef(manifest.branch)
      }`,
      {
        method: "PATCH",
        body: JSON.stringify({
          sha: commit.sha,
          force: false,
        }),
      },
    );
  } else {
    await githubJson(
      `/repos/${OWNER}/${REPO}/git/refs`,
      {
        method: "POST",
        body: JSON.stringify({
          ref: `refs/heads/${manifest.branch}`,
          sha: commit.sha,
        }),
      },
    );
  }

  let pullRequest: any = null;

  if (manifest.openPr !== false) {
    pullRequest = await cleanupExistingPr(manifest.branch);

    if (!pullRequest) {
      pullRequest = await githubJson(
        `/repos/${OWNER}/${REPO}/pulls`,
        {
          method: "POST",
          body: JSON.stringify({
            title: manifest.title ?? manifest.message,
            body: manifest.body ??
              "Automated change prepared through the Impetus GPT bridge.",
            head: manifest.branch,
            base: BASE_BRANCH,
            draft: manifest.draft !== false,
          }),
        },
      );
    }
  }

  return json({
    ok: true,
    branch: manifest.branch,
    commit: commit.sha,
    changedFiles: (manifest.files?.length ?? 0) +
      (manifest.delete?.length ?? 0),
    pullRequest: pullRequest
      ? {
        number: pullRequest.number,
        url: pullRequest.html_url,
        draft: pullRequest.draft,
      }
      : null,
  });
}

Deno.serve(async (req) => {
  try {
    const url = new URL(req.url);

    if (url.pathname === "/health") {
      return json({
        ok: true,
        service: "impetus-bridge",
        version: 1,
      });
    }

    if (!authorized(url)) {
      return json({
        ok: false,
        error: "Unauthorized",
      }, 401);
    }

    // GPT отправляет сюда небольшие base64url-куски.
    // Они сохраняются как не привязанные к ветке GitHub blobs.
    if (url.pathname === "/v1/stage") {
      return await stage(url);
    }

    // Manifest содержит только пути + SHA staged blobs.
    // Здесь собираются файлы, создаётся один commit,
    // fix-ветка и draft PR.
    if (url.pathname === "/v1/apply") {
      return await apply(url);
    }

    return json({
      ok: false,
      error: "Not found",
    }, 404);
  } catch (error) {
    if (error instanceof ApiError) {
      return json({
        ok: false,
        error: error.message,
      }, error.status);
    }

    console.error(error);

    return json({
      ok: false,
      error: "Internal error",
    }, 500);
  }
});
