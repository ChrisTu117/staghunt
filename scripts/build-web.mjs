// Bundle web/src into docs/app.js and stamp its hash into docs/index.html so
// browsers (and GitHub Pages' CDN) never serve a stale bundle.
import { build } from "esbuild";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";

await build({
  entryPoints: ["web/src/main.js"],
  bundle: true,
  format: "esm",
  minify: true,
  sourcemap: false,
  target: "es2020",
  inject: ["web/src/shims.js"],
  outfile: "docs/app.js",
  logLevel: "info",
});
const hash = createHash("sha256").update(readFileSync("docs/app.js")).digest("hex").slice(0, 10);
const html = readFileSync("docs/index.html", "utf8").replace(/src="app\.js(\?v=[0-9a-f]+)?"/, `src="app.js?v=${hash}"`);
writeFileSync("docs/index.html", html);
console.log(`docs/index.html -> app.js?v=${hash}`);
