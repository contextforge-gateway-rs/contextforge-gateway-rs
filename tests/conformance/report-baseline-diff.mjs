import { promises as fs } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "../..");
const suiteDir =
  process.env.MCP_CONFORMANCE_SUITE_DIR ??
  path.join(repoRoot, ".conformance-suite");
const specVersion = process.env.MCP_CONFORMANCE_SPEC_VERSION ?? "2026-07-28";
const sentinelCheck = "__contextforge_reporter_sentinel__";

const colors = colorEnabled()
  ? {
      bold: "\x1b[1m",
      dim: "\x1b[2m",
      red: "\x1b[31m",
      green: "\x1b[32m",
      yellow: "\x1b[33m",
      cyan: "\x1b[36m",
      reset: "\x1b[0m",
    }
  : { bold: "", dim: "", red: "", green: "", yellow: "", cyan: "", reset: "" };

function colorEnabled() {
  if (process.env.NO_COLOR) return false;
  const mode =
    process.env.MCP_CONFORMANCE_COLOR ?? process.env.CARGO_TERM_COLOR ?? "auto";
  if (mode === "always") return true;
  if (mode === "never") return false;
  if (mode === "auto")
    return Boolean(process.stdout.isTTY && process.env.TERM !== "dumb");
  throw new Error(
    `MCP_CONFORMANCE_COLOR must be auto, always, or never; got: ${mode}`,
  );
}

function usage(stream = process.stdout) {
  stream.write(`Usage: report-baseline-diff.sh [--bless] [results-dir [baseline-file [upstream-file]]]

Compare scored MCP conformance checks with the expected-failure baseline.
With --bless, replace the baseline with the current dataplane-owned findings.
`);
}

function row(color, label, message) {
  console.log(
    `  ${color}${colors.bold}${label.padStart(10)}${colors.reset} ${message}`,
  );
}

function annotation(title, message) {
  if (process.env.GITHUB_ACTIONS === "true")
    console.log(`::error title=${title}::${message}`);
}

function sortedUnique(values) {
  return [...new Set(values)].sort((left, right) =>
    left.localeCompare(right, "en"),
  );
}

function failed(check) {
  return check.status === "FAILURE" || check.status === "WARNING";
}

function parseArgs() {
  const args = process.argv.slice(2);
  if (args[0] === "--help" || args[0] === "-h") {
    usage();
    process.exit(0);
  }
  const bless = args[0] === "--bless";
  if (bless) args.shift();
  if (args.length > 3) {
    usage(process.stderr);
    process.exit(2);
  }
  return {
    bless,
    resultsDir: args[0] ?? path.join(repoRoot, "conformance-results"),
    baselineFile: args[1] ?? path.join(scriptDir, "expected-failures.yml"),
    upstreamFile:
      args[2] ?? path.join(scriptDir, "upstream-fixture-failures.yml"),
  };
}

async function suiteModules() {
  const source = (file) => pathToFileURL(path.join(suiteDir, "src", file)).href;
  const baseline = await import(source("expected-failures.ts"));
  const checks = await import(source("checks/collapse.ts"));
  const requirements = await import(source("requirements.ts"));
  return { ...baseline, ...checks, ...requirements };
}

async function loadResults(
  resultsDir,
  scoredScenarios,
  collapseDuplicateChecks,
) {
  let directories;
  try {
    directories = await fs.readdir(resultsDir, { withFileTypes: true });
  } catch (error) {
    if (error.code === "ENOENT")
      throw new Error(`No results directory: ${resultsDir}`);
    throw error;
  }

  const grouped = new Map();
  const resultPattern =
    /^server-(.*)-[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}-[0-9]{2}-[0-9]{2}-[0-9]{3}Z$/;
  for (const directory of directories) {
    if (!directory.isDirectory()) continue;
    const match = directory.name.match(resultPattern);
    if (!match || !scoredScenarios.has(match[1])) continue;

    const checksFile = path.join(resultsDir, directory.name, "checks.json");
    const checks = JSON.parse(await fs.readFile(checksFile, "utf8"));
    grouped.set(match[1], [...(grouped.get(match[1]) ?? []), ...checks]);
  }

  const results = [...grouped].map(([scenario, checks]) => ({
    scenario,
    checks: collapseDuplicateChecks(checks),
  }));
  results.sort((left, right) =>
    left.scenario.localeCompare(right.scenario, "en"),
  );
  if (results.length === 0) {
    throw new Error(
      `No scored conformance results were found in ${resultsDir}`,
    );
  }
  return results;
}

function filterUpstream(results, upstreamEntries, formatEntry) {
  const whole = new Set(
    upstreamEntries
      .filter((entry) => !entry.checkId)
      .map((entry) => entry.scenario),
  );
  const checks = new Set(
    upstreamEntries.filter((entry) => entry.checkId).map(formatEntry),
  );
  const matches = [];

  const ownedResults = results.map((result) => ({
    scenario: result.scenario,
    checks: result.checks.filter((check) => {
      if (!failed(check)) return true;
      const key = `${result.scenario}:${check.id}`;
      if (!whole.has(result.scenario) && !checks.has(key)) return true;
      matches.push(key);
      return false;
    }),
  }));
  return { ownedResults, upstreamMatches: sortedUnique(matches) };
}

async function blessBaseline(file, results, currentEntries, formatEntry) {
  const findings = sortedUnique(
    results.flatMap((result) =>
      result.checks
        .filter(failed)
        .map((check) => `${result.scenario}:${check.id}`),
    ),
  );
  const current = sortedUnique(currentEntries.map(formatEntry));
  if (JSON.stringify(findings) === JSON.stringify(current)) return false;

  const lines = [
    "# Generated by `make conformance-bless` from scored dataplane findings.",
    "# Pinned fixture findings are excluded; see upstream-fixture-failures.yml.",
    findings.length === 0
      ? "server: []"
      : `server:\n${findings.map((entry) => `  - ${entry}`).join("\n")}`,
    "",
  ];
  const temporary = `${file}.tmp-${process.pid}`;
  try {
    await fs.writeFile(temporary, lines.join("\n"));
    await fs.rename(temporary, file);
  } finally {
    await fs.rm(temporary, { force: true });
  }
  return true;
}

function evaluateDetailed(results, baselineEntries, evaluateBaseline) {
  const whole = new Set(
    baselineEntries
      .filter((entry) => !entry.checkId)
      .map((entry) => entry.scenario),
  );
  const sentinels = results
    .filter((result) => !whole.has(result.scenario))
    .map((result) => ({ scenario: result.scenario, checkId: sentinelCheck }));
  const evaluation = evaluateBaseline(results, [
    ...baselineEntries,
    ...sentinels,
  ]);
  return {
    expected: sortedUnique(evaluation.expectedFailures),
    unexpected: sortedUnique(evaluation.unexpectedFailures),
    stale: sortedUnique(
      evaluation.staleEntries.filter(
        (entry) => !entry.endsWith(`:${sentinelCheck}`),
      ),
    ),
  };
}

async function writeSummary({
  passCount,
  expected,
  upstream,
  unexpected,
  stale,
  skipCount,
  bless,
}) {
  if (!process.env.GITHUB_STEP_SUMMARY) return;
  const lines = [
    `## MCP ${specVersion} conformance`,
    "",
    "| Outcome | Count |",
    "| --- | ---: |",
    `| Scored checks passed | ${passCount} |`,
    `| Expected failures reproduced | ${expected.length} |`,
    `| Pinned fixture findings ignored | ${upstream.length} |`,
    `| Expected pass, got failure | ${unexpected.length} |`,
    `| Expected failure, got pass | ${stale.length} |`,
    `| Skipped checks | ${skipCount} |`,
  ];
  if (unexpected.length > 0) {
    lines.push(
      "",
      "### Expected pass, got failure",
      ...unexpected.map((entry) => `- \`${entry}\``),
    );
  }
  if (stale.length > 0) {
    lines.push(
      "",
      "### Expected failure, got pass",
      ...stale.map((entry) => `- \`${entry}\``),
    );
  }
  const clean = unexpected.length === 0 && stale.length === 0;
  lines.push(
    "",
    bless
      ? `✅ Expected-failure baseline updated with ${expected.length} dataplane findings.`
      : clean
        ? "✅ Actual dataplane findings match the expected-failure baseline."
        : "❌ Actual dataplane findings do not match the expected-failure baseline.",
    "",
  );
  await fs.appendFile(process.env.GITHUB_STEP_SUMMARY, lines.join("\n"));
}

async function reportError(error) {
  const message = error instanceof Error ? error.message : String(error);
  row(colors.red, "ERROR", message);
  annotation("Conformance report failed", message);
  if (process.env.GITHUB_STEP_SUMMARY) {
    await fs.appendFile(
      process.env.GITHUB_STEP_SUMMARY,
      `## MCP ${specVersion} conformance\n\n❌ ${message}\n`,
    );
  }
}

async function main() {
  const options = parseArgs();
  const suite = await suiteModules();
  const requirements = suite.loadRequirements(specVersion);
  const results = await loadResults(
    options.resultsDir,
    new Set(suite.scoredScenarios(requirements, "server")),
    suite.collapseDuplicateChecks,
  );
  const upstream =
    (await suite.loadExpectedFailures(options.upstreamFile)).server ?? [];
  let baseline =
    (await suite.loadExpectedFailures(options.baselineFile)).server ?? [];
  const { ownedResults, upstreamMatches } = filterUpstream(
    results,
    upstream,
    suite.formatEntry,
  );

  const blessChanged =
    options.bless &&
    (await blessBaseline(
      options.baselineFile,
      ownedResults,
      baseline,
      suite.formatEntry,
    ));
  if (blessChanged)
    baseline =
      (await suite.loadExpectedFailures(options.baselineFile)).server ?? [];

  const evaluation = evaluateDetailed(
    ownedResults,
    baseline,
    suite.evaluateBaseline,
  );
  const allChecks = results.flatMap((result) => result.checks);
  const passCount = allChecks.filter(
    (check) => check.status === "SUCCESS",
  ).length;
  const skipCount = allChecks.filter(
    (check) => check.status === "SKIPPED",
  ).length;
  const status = new Map(
    ownedResults.flatMap((result) =>
      result.checks
        .filter(failed)
        .map((check) => [`${result.scenario}:${check.id}`, check.status]),
    ),
  );

  console.log(
    `\n${colors.bold}MCP conformance${colors.reset} ${colors.dim}(${specVersion})${colors.reset}`,
  );
  row(colors.green, "PASS", `${passCount} scored checks passed`);
  for (const entry of evaluation.expected) {
    row(
      colors.yellow,
      "XFAIL",
      `${entry} ${colors.dim}(expected failure reproduced)${colors.reset}`,
    );
  }
  for (const entry of upstreamMatches) {
    row(
      colors.cyan,
      "UPSTREAM",
      `${entry} ${colors.dim}(ignored pinned-fixture finding)${colors.reset}`,
    );
  }
  for (const entry of evaluation.unexpected) {
    row(
      colors.red,
      "FAIL",
      `${entry} ${colors.dim}(expected PASS, got ${status.get(entry) ?? "FAILURE"})${colors.reset}`,
    );
    annotation("Expected conformance pass failed", entry);
  }
  for (const entry of evaluation.stale) {
    row(
      colors.red,
      "XPASS",
      `${entry} ${colors.dim}(expected FAILURE, got PASS)${colors.reset}`,
    );
    annotation("Expected conformance failure passed", entry);
  }
  if (skipCount > 0)
    row(colors.dim, "SKIP", `${skipCount} scored checks skipped`);
  if (options.bless) {
    row(
      colors.green,
      "BLESS",
      blessChanged
        ? `updated ${options.baselineFile}`
        : `${options.baselineFile} was already current`,
    );
  }

  console.log(
    `\n${colors.bold}Summary${colors.reset}: ${passCount} passed, ${evaluation.expected.length} expected failures, ` +
      `${upstreamMatches.length} upstream findings ignored, ${evaluation.unexpected.length} failed, ` +
      `${evaluation.stale.length} unexpected passes`,
  );
  await writeSummary({
    passCount,
    expected: evaluation.expected,
    upstream: upstreamMatches,
    unexpected: evaluation.unexpected,
    stale: evaluation.stale,
    skipCount,
    bless: options.bless,
  });

  if (
    !options.bless &&
    (evaluation.unexpected.length > 0 || evaluation.stale.length > 0)
  )
    process.exitCode = 1;
}

main().catch(async (error) => {
  await reportError(error);
  process.exitCode = 2;
});
