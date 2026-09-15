import assert from "node:assert/strict";
import test from "node:test";

import {
  getPublishedDesktopVersions,
  renderChangelogModule,
} from "./changelog-build.ts";

const published = {
  tag_name: "desktop_v1.4.23",
  draft: false,
  prerelease: false,
  published_at: "2026-09-08T11:12:17Z",
};

test("accepts only actually published stable desktop releases", async () => {
  const versions = await getPublishedDesktopVersions(async () =>
    Response.json([
      published,
      { ...published, tag_name: "desktop_v1.4.24", draft: true },
      { ...published, tag_name: "desktop_v1.4.25", prerelease: true },
      { ...published, tag_name: "desktop_v1.4.26", published_at: null },
      { ...published, tag_name: "desktop_v1.4.27", published_at: "invalid" },
      { ...published, tag_name: "desktop_nightly_v1.4.24-nightly.4" },
      { ...published, tag_name: "cli_v1.4.24" },
      { tag_name: "desktop_v1.4.28" },
      null,
    ]),
  );
  assert.deepEqual([...versions], ["1.4.23"]);
});

test("includes older releases across pages without following arbitrary URLs", async () => {
  const urls: string[] = [];
  const versions = await getPublishedDesktopVersions(async (url) => {
    urls.push(String(url));
    return urls.length === 1
      ? Response.json([published], {
          headers: { link: '<https://example.com>; rel="next"' },
        })
      : Response.json([{ ...published, tag_name: "desktop_v1.0.0" }]);
  });
  assert.deepEqual([...versions], ["1.4.23", "1.0.0"]);
  assert.deepEqual(
    urls,
    [1, 2].map(
      (page) =>
        `https://api.github.com/repos/fastrepl/anarlog/releases?per_page=100&page=${page}`,
    ),
  );
});

test("fails the build rather than exposing drafts when publication cannot be checked", async () => {
  await assert.rejects(
    getPublishedDesktopVersions(
      async () => new Response(null, { status: 429 }),
    ),
    /429/,
  );
  await assert.rejects(
    getPublishedDesktopVersions(async () =>
      Response.json({ message: "invalid" }),
    ),
    /Invalid/,
  );
  await assert.rejects(
    getPublishedDesktopVersions(async () => {
      throw new Error("offline");
    }),
    /offline/,
  );
});

test("unreleased notes are absent from the website module, including its raw imports", () => {
  const files = [
    "/content/1.4.23.md",
    "/content/1.4.24.md",
    "/content/nightly.md",
    "/content/AGENTS.md",
  ];
  const module = renderChangelogModule(files, new Set(["1.4.23"]));
  assert.match(module, /1\.4\.23\.md\?raw/);
  assert.doesNotMatch(module, /1\.4\.24|nightly|AGENTS/);
  assert.equal(renderChangelogModule(files, new Set()), "export default {};");
  assert.match(
    renderChangelogModule(files, new Set(["1.4.23", "1.4.24"])),
    /1\.4\.24\.md\?raw/,
  );
});

test("local development can preview stable drafts but never Nightly or instruction files", () => {
  const module = renderChangelogModule(
    ["/content/1.4.24.md", "/content/nightly.md", "/content/AGENTS.md"],
    null,
  );
  assert.match(module, /1\.4\.24\.md\?raw/);
  assert.doesNotMatch(module, /nightly|AGENTS/);
});
