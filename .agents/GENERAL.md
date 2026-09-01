# General repository rules

## Maximum code file size

- Every hand-maintained source, test, and automation file must contain no more than 500 physical
  lines.
- Treat the limit as a design constraint: split files and logic by responsibility before they
  reach 500 lines so modules remain focused and easy to review, test, and maintain.
- Do not satisfy the limit by compressing code, removing useful documentation, or disabling the
  normal formatter.
- Generated artifacts, dependency lockfiles, vendored third-party code, licenses, and prose-only
  documentation or planning files are exempt. Ordinary application, test, and script code is not.
- Run `./scripts/check-file-sizes.sh` after adding or reorganizing code. The full workspace
  verifier runs this check automatically.
