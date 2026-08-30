import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const dist = new URL("../dist/index.html", import.meta.url);
const workflow = new URL("../../.github/workflows/pages.yml", import.meta.url);

test("the built landing page has the required product contract", async () => {
  const html = await readFile(dist, "utf8");

  assert.match(html, /<html lang="en">/);
  assert.match(html, /Give coding agents a runtime you can trust\./);
  assert.match(html, /aria-label="Impetus request control flow"/);
  assert.match(html, /id="capabilities"/);
  assert.match(html, /id="architecture"/);
  assert.match(html, /id="install"/);
  assert.match(html, /curl -fsSL https:\/\/raw\.githubusercontent\.com\/1tuz\/impetus\/main\/scripts\/install\.sh \| zsh/);
  assert.match(html, /doctor/);
  assert.match(html, /create/);
  assert.match(html, /prompt/);
  assert.match(html, /stream/);
  assert.match(html, /approve/);
  assert.match(html, /data-copy-command/);
  assert.doesNotMatch(html, /react/i);
});

test("the CSS preserves responsive and reduced-motion safeguards", async () => {
  const css = await readFile(new URL("../src/styles/global.css", import.meta.url), "utf8");

  assert.match(css, /@media \(max-width: 700px\)/);
  assert.match(css, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(css, /:focus-visible/);
  assert.match(css, /overflow-wrap/);
});

test("the Pages workflow builds and deploys the static site", async () => {
  const yaml = await readFile(workflow, "utf8");

  for (const action of [
    "actions/checkout",
    "actions/configure-pages",
    "actions/upload-pages-artifact",
    "actions/deploy-pages",
  ]) {
    assert.match(yaml, new RegExp(action));
  }
  assert.match(yaml, /working-directory: site/);
  assert.match(yaml, /contents: read/);
  assert.match(yaml, /pages: write/);
  assert.match(yaml, /id-token: write/);
});
