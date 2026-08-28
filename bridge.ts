const OWNER = "1tuz";
const REPO = "impetus";

function json(data: unknown, status = 200) {
  return new Response(JSON.stringify(data, null, 2), {
    status,
    headers: {
      "content-type": "application/json; charset=utf-8",
      "cache-control": "no-store",
    },
  });
}

async function github(
  path: string,
  options: RequestInit = {},
) {
  const token = Deno.env.get("GITHUB_TOKEN");

  if (!token) {
    throw new Error("GITHUB_TOKEN is not configured");
  }

  return await fetch(`https://api.github.com${path}`, {
    ...options,
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": "impetus-bridge",
      "content-type": "application/json",
      ...(options.headers ?? {}),
    },
  });
}

function authorized(url: URL) {
  const secret = Deno.env.get("BRIDGE_SECRET");
  const supplied = url.searchParams.get("key");

  return Boolean(secret && supplied && secret === supplied);
}

Deno.serve(async (req) => {
  try {
    const url = new URL(req.url);

    // Публичная проверка состояния.
    if (url.pathname === "/health") {
      return json({
        ok: true,
        service: "impetus-bridge",
      });
    }

    // Всё ниже требует BRIDGE_SECRET.
    if (!authorized(url)) {
      return json({
        ok: false,
        error: "Unauthorized",
      }, 401);
    }

    // Проверка GitHub.
    if (url.pathname === "/repo") {
      const response = await github(`/repos/${OWNER}/${REPO}`);

      if (!response.ok) {
        return json({
          ok: false,
          githubStatus: response.status,
        }, 502);
      }

      const repo = await response.json();

      return json({
        ok: true,
        repository: repo.full_name,
        defaultBranch: repo.default_branch,
      });
    }

    // Создание только безопасных fix/chatgpt-* веток.
    if (url.pathname === "/create-branch") {
      const branch = url.searchParams.get("branch");

      if (!branch || !/^fix\/chatgpt-[a-z0-9._-]+$/.test(branch)) {
        return json({
          ok: false,
          error: "Branch must match fix/chatgpt-*",
        }, 400);
      }

      // Получаем HEAD main.
      const mainResponse = await github(
        `/repos/${OWNER}/${REPO}/git/ref/heads/main`,
      );

      if (!mainResponse.ok) {
        return json({
          ok: false,
          error: "Cannot read main ref",
          githubStatus: mainResponse.status,
        }, 502);
      }

      const main = await mainResponse.json();
      const sha = main.object.sha;

      // Создаём fix-ветку.
      const createResponse = await github(
        `/repos/${OWNER}/${REPO}/git/refs`,
        {
          method: "POST",
          body: JSON.stringify({
            ref: `refs/heads/${branch}`,
            sha,
          }),
        },
      );

      const result = await createResponse.json();

      if (!createResponse.ok) {
        return json({
          ok: false,
          githubStatus: createResponse.status,
          error: result.message ?? "GitHub refused branch creation",
        }, 502);
      }

      return json({
        ok: true,
        branch,
        sha,
      });
    }

    return json({
      ok: false,
      error: "Not found",
    }, 404);
  } catch (error) {
    console.error(error);

    return json({
      ok: false,
      error: "Internal error",
    }, 500);
  }
});
