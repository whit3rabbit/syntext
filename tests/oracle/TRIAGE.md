# st ↔ rg Differential Oracle Triage Workflow

This document details the workflow for triaging failures discovered by the differential oracle testing suites (both local runs and nightly CI runs).

## Triage Steps

When a differential test fails, follow these steps:

### 1. Locate the Minimized Repro
- The test suite automatically runs a delta-debugger on the failure to minimize the file corpus, query, and flags.
- It will print a **MINIMIZED REPRODUCER** to the console (stdout/stderr).
- If run locally or if downloading the nightly CI run artifacts, the minimized test case is written to a JSON file under:
  `tests/oracle/regressions/repro_<hash>.json`

### 2. Reproduce the Failure Locally
If you downloaded a regression file from CI:
1. Place it in `tests/oracle/regressions/`.
2. Run the regression test suite locally:
   ```bash
   cargo test --test oracle_cli test_regression_fixtures --features oracle -- --nocapture
   ```
This test runs every JSON file in the regressions folder. It should fail on the new file, reproducing the bug immediately.

### 3. Analyze the Failure

Decide if the divergence is a **real bug** or a **legitimate divergence**:

#### Option A: It is a Real Bug
1. Leave the JSON regression file in `tests/oracle/regressions/` so it remains part of the test suite.
2. Fix the bug in the `syntext` codebase.
3. Verify the fix by running:
   ```bash
   cargo test --features oracle
   ```
4. Commit both your code fix and the regression JSON file. This guarantees the bug will never regress.

#### Option B: It is a Legitimate Divergence
If `st` and `rg` differ due to intentional design choices (e.g., smart-case differences, unsupported regex syntax, or platform-specific walk differences):
1. Document the divergence in [tests/oracle/DIVERGENCES.md](file:///Users/whit3rabbit/Documents/GitHub/syntext/tests/oracle/DIVERGENCES.md) with a written justification.
2. Update the proptest query/flag generators in `tests/integration/oracle_helpers.rs` to filter out this specific pattern/flag combination so it is not generated in future runs.
3. Delete the temporary regression JSON file from `tests/oracle/regressions/`.

## One-Person-Project Rule
> [!IMPORTANT]
> **Never suppress a failure without documenting a written reason.** If a test fails, either fix the code or explicitly add the case/behavior to `DIVERGENCES.md` and explain why `st` and `rg` are allowed to disagree.
