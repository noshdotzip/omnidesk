/**
 * Fail fast when the JavaScript toolchain is running under CPU emulation.
 *
 * On Windows on ARM, an x64 package manager runs under Prism (the x64 translation
 * layer). It reports `process.arch === "x64"`, so npm/pnpm resolve every `cpu`-gated
 * optional dependency to the x64 variant: `@esbuild/win32-x64` (an x64 *executable*
 * that then runs emulated on every build) and `@rollup/rollup-win32-x64-msvc` (a
 * native addon that native ARM64 Node cannot load at all, forcing rollup onto its
 * slow JS fallback). Both are silent — the install succeeds and nothing warns.
 *
 * Ultidesk targets Windows ARM64 *natively* (ADR-0008), so this is a hard error
 * rather than a warning. Override with ULTIDESK_ALLOW_EMULATED_TOOLCHAIN=1 if you
 * deliberately want an emulated install.
 */

const ESCAPE_HATCH = "ULTIDESK_ALLOW_EMULATED_TOOLCHAIN";

/**
 * The true CPU architecture of the machine, independent of any emulation the current
 * process is subject to.
 *
 * `PROCESSOR_ARCHITECTURE` is rewritten to `AMD64` for emulated processes, and unlike
 * WOW64, Prism does *not* set `PROCESSOR_ARCHITEW6432`. `PROCESSOR_IDENTIFIER` is left
 * intact ("ARMv8 (64-bit) Family 8 ... Qualcomm Technologies Inc"), so it is the one
 * env var that still tells the truth from inside an emulated process.
 */
function trueHostArch() {
  const identifier = process.env["PROCESSOR_IDENTIFIER"] ?? "";
  if (/\bARM(v?\d+)?\b/i.test(identifier)) return "arm64";
  const declared = process.env["PROCESSOR_ARCHITECTURE"] ?? "";
  if (/ARM64/i.test(declared)) return "arm64";
  return "x64";
}

/**
 * The architecture of the *package manager*, which is what actually selects
 * `cpu`-gated packages. It is not necessarily this script's own `process.arch`:
 * pnpm's standalone launcher bundles its own Node, but runs lifecycle scripts with
 * whatever `node` is on PATH — which may well be native while the launcher is not.
 * Every npm-compatible client publishes its own view as the trailing
 * "<platform> <arch>" of the user-agent string.
 */
function packageManagerArch() {
  const ua = process.env["npm_config_user_agent"];
  if (!ua) return null;
  const match = /\b(win32|darwin|linux)\s+(\S+)/.exec(ua);
  return match ? match[2] : null;
}

function main() {
  if (process.platform !== "win32") return;

  const host = trueHostArch();
  if (host !== "arm64") return; // Nothing to police on an x64 machine.

  const pm = packageManagerArch();
  const emulated = [
    ["package manager", pm],
    ["node", process.arch],
  ].filter(([, arch]) => arch !== null && arch !== "arm64");

  if (emulated.length === 0) return;

  const detail = emulated.map(([what, arch]) => `  - ${what} is ${arch}`).join("\n");
  const message = [
    "",
    "Ultidesk: emulated toolchain detected on Windows ARM64.",
    "",
    `This machine is ARM64 (${process.env["PROCESSOR_IDENTIFIER"] ?? "unknown CPU"}), but:`,
    detail,
    "",
    "Installing from an emulated client silently selects x64 native packages",
    "(@esbuild/win32-x64, @rollup/rollup-win32-x64-msvc). Those either run under",
    "Prism on every build or cannot be loaded by native ARM64 Node at all.",
    "",
    "Fix: run pnpm through Corepack, which uses the native ARM64 Node on PATH:",
    "",
    "    corepack enable",
    "    corepack pnpm install",
    "",
    `Override (not recommended): set ${ESCAPE_HATCH}=1`,
    "",
  ].join("\n");

  if (process.env[ESCAPE_HATCH] === "1") {
    console.warn(message.replace("Ultidesk:", "Ultidesk (WARNING, override set):"));
    return;
  }
  console.error(message);
  process.exit(1);
}

main();
