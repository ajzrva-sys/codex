import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  copyFileSync,
  writeFileSync,
  rmSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";

const source = fileURLToPath(new URL("../bin/codex.js", import.meta.url));

function launch(t, { arch = "x64", target, optionalPackage = false } = {}) {
  const root = mkdtempSync(path.join(os.tmpdir(), "codex-freebsd-launcher-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const launcher = path.join(root, "bin", "codex.js");
  mkdirSync(path.dirname(launcher), { recursive: true });
  copyFileSync(source, launcher);
  writeFileSync(path.join(root, "package.json"), '{"type":"module"}');

  if (target) {
    const packageRoot = optionalPackage
      ? path.join(root, "node_modules", "@openai", `codex-freebsd-${arch}`)
      : root;
    const bin = path.join(packageRoot, "vendor", target, "bin");
    mkdirSync(bin, { recursive: true });
    if (optionalPackage) {
      writeFileSync(path.join(packageRoot, "package.json"), "{}");
    }
    writeFileSync(
      path.join(bin, "codex"),
      '#!/bin/sh\nprintf "%s\\n" "$@"\nexit 23\n',
      { mode: 0o755 },
    );
  }

  return spawnSync(
    process.execPath,
    [
      "--input-type=module",
      "--eval",
      `Object.defineProperty(process, "platform", { value: "freebsd" });
       Object.defineProperty(process, "arch", { value: ${JSON.stringify(arch)} });
       process.argv = [process.execPath, ${JSON.stringify(launcher)}, "exec", "argument with spaces"];
       await import(${JSON.stringify(pathToFileURL(launcher).href)});`,
    ],
    { encoding: "utf8" },
  );
}

for (const [arch, target] of [
  ["x64", "x86_64-unknown-freebsd"],
  ["arm64", "aarch64-unknown-freebsd"],
]) {
  for (const optionalPackage of [false, true]) {
    test(`FreeBSD ${arch} launches ${optionalPackage ? "optional package" : "local vendor"} and preserves arguments and exit status`, (t) => {
      const result = launch(t, { arch, target, optionalPackage });
      assert.equal(result.error, undefined);
      assert.equal(result.stderr, "");
      assert.equal(result.stdout, "exec\nargument with spaces\n");
      assert.equal(result.status, 23);
    });
  }
}

test("missing FreeBSD binary explains how to obtain a native build", (t) => {
  const result = launch(t);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Missing native FreeBSD binary:/);
  assert.match(result.stderr, /scripts\/freebsd\/README.md/);
});

test("unsupported FreeBSD architectures fail before starting a binary", (t) => {
  const result = launch(t, { arch: "riscv64" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Unsupported platform: freebsd \(riscv64\)/);
});
