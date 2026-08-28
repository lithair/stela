// Browser tests, and only for what needs a browser.
//
// probatum owns HTTP behaviour and covers it thoroughly; re-asserting any of it
// here would buy nothing and cost the flakiness and the seconds that browser
// tests are known for. What lives here is the surface probatum cannot reach at
// all: the editor's JavaScript, and the human path through the login form.
//
// That gap is not theoretical. The one bug in this project that the checks
// missed was "no human can log in" — the admin route answered 401 and nothing
// served a form, while every check passed because they POST JSON by hand.

const { defineConfig } = require("@playwright/test");

const PORT = 3300;
const BLOG = "/tmp/e2e-blog";
const STELA = "./target/x86_64-unknown-linux-musl/release/stela";

module.exports = defineConfig({
  testDir: "./e2e",
  // No retries: a browser test that only passes sometimes is telling you
  // something, and hiding it behind a retry is how a suite stops meaning
  // anything.
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    trace: "retain-on-failure",
  },
  webServer: {
    // Scaffolded fresh, then served from it — the same path an operator walks.
    // The credentials are written out because the spec needs them and the
    // command prints them exactly once.
    command:
      `rm -rf ${BLOG} && ${STELA} new ${BLOG} > ${BLOG}-creds.txt && ` +
      `cd ${BLOG} && ${process.cwd()}/${STELA} serve --port ${PORT} --host 127.0.0.1`,
    url: `http://127.0.0.1:${PORT}/`,
    reuseExistingServer: false,
    stdout: "pipe",
    stderr: "pipe",
  },
});
