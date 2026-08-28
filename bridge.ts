function json(data: unknown, status = 200) {
  return new Response(JSON.stringify(data, null, 2), {
    status,
    headers: {
      "content-type": "application/json; charset=utf-8",
    },
  });
}

Deno.serve(async (req) => {
  const url = new URL(req.url);

  if (url.pathname === "/health") {
    return json({
      ok: true,
      service: "impetus-bridge",
    });
  }

  if (url.pathname === "/repo") {
    const githubToken = Deno.env.get("GITHUB_TOKEN");
    const bridgeSecret = Deno.env.get("BRIDGE_SECRET");

    if (!githubToken) {
      return json({ ok: false, error: "GITHUB_TOKEN is not configured" }, 500);
    }

    const response = await fetch(
      "https://api.github.com/repos/1tuz/impetus",
      {
        headers: {
          Authorization: `Bearer ${githubToken}`,
          Accept: "application/vnd.github+json",
          "X-GitHub-Api-Version": "2022-11-28",
          "User-Agent": "impetus-bridge",
        },
      },
    );

    if (!response.ok) {
      return json({
        ok: false,
        githubStatus: response.status,
        error: "GitHub API request failed",
      }, 502);
    }

    const repo = await response.json();

    return json({
      ok: true,
      githubAuth: true,
      bridgeSecretConfigured: Boolean(bridgeSecret),
      repository: repo.full_name,
      defaultBranch: repo.default_branch,
    });
  }

  return json({ ok: false, error: "Not found" }, 404);
});
