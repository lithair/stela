// The human path: sign in, write a post, see it on the site.
//
// Everything here goes through the rendered page and its JavaScript. Nothing
// here re-asserts an HTTP status probatum already owns.

const { test, expect } = require("@playwright/test");
const fs = require("fs");

// `stela new` prints the route and the password once; the config keeps only a
// hash, so the credentials come from what the command said, not from the blog.
function credentials() {
  const text = fs.readFileSync("/tmp/e2e-blog-creds.txt", "utf8");
  return {
    route: text.match(/Admin route: (\S+)/)[1],
    password: text.match(/Password: (\S+)/)[1],
    username: text.match(/Username: (\S+)/)[1],
  };
}

async function signIn(page) {
  const { route, username, password } = credentials();
  await page.goto(`${route}/login`);
  await page.fill('input[name="username"]', username);
  await page.fill('input[name="password"]', password);
  await page.click('button[type="submit"]');
  await page.waitForURL(`**${route}`);
  return route;
}

test("the login form is what a person actually gets", async ({ page }) => {
  const { route } = credentials();

  // Landing on the panel without a session must not be a dead end. This is the
  // exact bug that shipped once: 401 and no way forward.
  await page.goto(`${route}/login`);

  await expect(page.locator('input[name="username"]')).toBeVisible();
  await expect(page.locator('input[name="password"]')).toBeVisible();
  await expect(page.locator('button[type="submit"]')).toBeVisible();
});

test("wrong credentials say so without saying which half was wrong", async ({ page }) => {
  const { route, username } = credentials();
  await page.goto(`${route}/login`);
  await page.fill('input[name="username"]', username);
  await page.fill('input[name="password"]', "definitely-not-it");
  await page.click('button[type="submit"]');

  const status = page.locator("#status");
  await expect(status).toContainText("refused");
  // Naming the username or the password would hand an attacker half the answer.
  await expect(status).not.toContainText(/username|user name/i);
});

test("signing in leads to the editor", async ({ page }) => {
  await signIn(page);
  await expect(page.locator('textarea[name="body"]')).toBeVisible();
});

test("writing a post publishes it to the public site", async ({ page }) => {
  await signIn(page);

  await page.fill('input[name="slug"]', "ecrit-au-navigateur");
  await page.fill('input[name="title"]', "Écrit au navigateur");
  await page.fill('textarea[name="body"]', "# Bonjour\n\nUn vrai paragraphe.");
  await page.check('input[name="published"]');
  await page.click('button[type="submit"]');

  // The page reloads once the write and the rebuild have both succeeded, so the
  // post appearing in the list is the signal that the whole chain worked.
  await expect(page.locator("text=Écrit au navigateur")).toBeVisible();

  // And it is genuinely on the site, not just in the panel.
  await page.goto("/posts/ecrit-au-navigateur");
  await expect(page.locator("article h1")).toContainText("Bonjour");
  await expect(page.locator("text=Un vrai paragraphe.")).toBeVisible();
});

test("clicking a post loads it back into the form", async ({ page }) => {
  const route = await signIn(page);

  // Written here rather than leaning on another test: Playwright runs these
  // independently and in any order, so a test that needs a post creates one.
  await page.fill('input[name="slug"]', "a-relire");
  await page.fill('input[name="title"]', "À relire");
  await page.fill('textarea[name="body"]', "# Le corps\n\nÀ retrouver dans le formulaire.");
  await page.check('input[name="published"]');
  await page.click('button[type="submit"]');
  await expect(page.locator("text=À relire")).toBeVisible();

  // The posts are embedded as JSON so a click fills the form with no round
  // trip. Nothing but a browser can tell whether that actually works.
  await page.goto(route);
  await page.click('[data-slug="a-relire"]');

  await expect(page.locator('input[name="title"]')).toHaveValue("À relire");
  // toHaveValue, not toContainText: the script sets .value, and a textarea's
  // DOM text content is what it was rendered with, which is nothing.
  await expect(page.locator('textarea[name="body"]')).toHaveValue(/Le corps/);
});

test("a draft stays off the public site", async ({ page }) => {
  await signIn(page);

  await page.fill('input[name="slug"]', "brouillon-navigateur");
  await page.fill('input[name="title"]', "Pas encore fini");
  await page.fill('textarea[name="body"]', "# Wip");
  // published deliberately left unchecked
  await page.click('button[type="submit"]');
  await expect(page.locator("text=Pas encore fini")).toBeVisible();

  const response = await page.goto("/posts/brouillon-navigateur");
  expect(response.status()).toBe(404);
});

test("logging out really ends the session", async ({ page }) => {
  const route = await signIn(page);

  await page.click("#logout");
  await page.waitForURL("**/");

  // Back to the panel: the cookie is gone, so this must not open.
  const response = await page.goto(route);
  expect(response.status()).toBe(401);
});
