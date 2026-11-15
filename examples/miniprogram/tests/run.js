const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const files = fs.readdirSync(__dirname)
  .filter((name) => name.endsWith(".test.js"))
  .sort()
  .map((name) => path.join(__dirname, name));

if (!files.length) {
  console.error("No .test.js files found.");
  process.exitCode = 1;
} else {
  const options = process.argv.slice(2);
  if (!options.some((option) => option.startsWith("--test-reporter"))) {
    options.unshift("--test-reporter=dot");
  }
  const result = spawnSync(process.execPath, ["--test", ...options, ...files], {
    cwd: path.resolve(__dirname, ".."),
    stdio: "inherit",
  });
  if (result.error) console.error(result.error.message);
  process.exitCode = result.status === null ? 1 : result.status;
}